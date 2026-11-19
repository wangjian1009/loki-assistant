import AppKit
import Foundation
import Observation

struct MonitoredEmacsBuffer: Decodable, Equatable, Sendable {
    let name: String
    let mode: String
    let directory: String
    let file: String?
    let modified: Bool
}

struct EmacsBufferStatus: Decodable, Sendable {
    struct AppStatus: Decodable, Sendable {
        let buffers: [MonitoredEmacsBuffer]?
    }

    let apiVersion: String
    let status: [String: AppStatus]
    let updatedAt: UInt64?

    var bufferGroups: [EmacsBufferGroup] {
        status.keys.sorted().compactMap { app in
            guard let buffers = status[app]?.buffers, !buffers.isEmpty else { return nil }
            return EmacsBufferGroup(app: app, buffers: buffers)
        }
    }

    enum CodingKeys: String, CodingKey {
        case apiVersion = "api_version"
        case status
        case updatedAt = "updated_at"
    }
}

struct EmacsBufferGroup: Equatable, Identifiable, Sendable {
    let app: String
    let buffers: [MonitoredEmacsBuffer]
    var id: String { app }
}

private struct EmacsBufferActivation: Decodable {
    let api_version: String
    let activation: String
}

struct EmacsReportedStatusClient: Sendable {
    typealias Transport = @Sendable (URLRequest) async throws -> (Data, URLResponse)
    private let discover: @Sendable () throws -> ServiceEndpoint
    private let transport: Transport

    init(
        discover: @escaping @Sendable () throws -> ServiceEndpoint = { try NotificationReceiptClient.discoverEndpoint() },
        transport: @escaping Transport = { try await URLSession.shared.data(for: $0) }
    ) {
        self.discover = discover
        self.transport = transport
    }

    private func endpoint(operation: CenterLogOperation? = nil) async throws -> ServiceEndpoint {
        let endpoint = try discover()
        let request = URLRequest(url: try NotificationReceiptClient.url(endpoint: endpoint, path: "/healthz"),
                                 cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 5)
        let (data, response): (Data, URLResponse)
        do {
            (data, response) = try await transport(request)
        } catch {
            operation?.call("GET /healthz", result: "failure", details: ["error": error.localizedDescription])
            throw error
        }
        let statusCode = (response as? HTTPURLResponse)?.statusCode ?? 0
        operation?.call("GET /healthz", result: statusCode == 200 ? "success" : "failure", details: ["http_status": "\(statusCode)"])
        guard (response as? HTTPURLResponse)?.statusCode == 200,
              let health = try? JSONDecoder().decode(HealthResponse.self, from: data),
              health.service == "loki-services", health.status == "ok" else {
            throw NotificationReceiptError.serviceUnavailable
        }
        try Task.checkCancellation()
        return endpoint
    }

    private func send(_ request: URLRequest, operation: CenterLogOperation? = nil) async throws -> Data {
        let action = "\(request.httpMethod ?? "GET") \(request.url?.path ?? "")"
        let (data, response): (Data, URLResponse)
        do {
            (data, response) = try await transport(request)
        } catch {
            operation?.call(action, result: "failure", details: ["error": error.localizedDescription])
            throw error
        }
        guard let response = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        operation?.call(action, result: response.statusCode == 200 ? "success" : "failure", details: ["http_status": "\(response.statusCode)"])
        guard response.statusCode == 200 else {
            let message = (try? JSONDecoder().decode(ServiceAPIErrorResponse.self, from: data))?.error.message
            throw EmacsReportedStatusError.failed(message ?? "Emacs 状态接口返回 HTTP \(response.statusCode)")
        }
        return data
    }

    func fetchBuffers(operation: CenterLogOperation? = nil) async throws -> EmacsBufferStatus {
        let endpoint = try discover()
        let request = URLRequest(url: try NotificationReceiptClient.url(endpoint: endpoint, path: "/status"),
                                 cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 5)
        let result = try JSONDecoder().decode(EmacsBufferStatus.self, from: await send(request, operation: operation))
        guard result.apiVersion == "1",
              result.bufferGroups.allSatisfy({ group in
                  !group.app.isEmpty
                      && group.buffers.allSatisfy { !$0.name.isEmpty }
              }) else {
            throw EmacsReportedStatusError.failed("Emacs buffer 状态响应无效")
        }
        return result
    }

    @MainActor
    func makeStatusModel() -> ServiceStatusModel {
        ServiceStatusModel(discover: discover, transport: transport)
    }

    func activate(_ buffer: MonitoredEmacsBuffer, operation: CenterLogOperation? = nil) async throws {
        let endpoint = try await endpoint(operation: operation)
        let base = try NotificationReceiptClient.url(endpoint: endpoint, path: "/emacs/activate")
        var components = URLComponents(url: base, resolvingAgainstBaseURL: false)!
        components.queryItems = [URLQueryItem(name: "buffer", value: buffer.name)]
        var request = URLRequest(url: components.url!, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 35)
        request.httpMethod = "POST"
        let result = try JSONDecoder().decode(EmacsBufferActivation.self, from: await send(request, operation: operation))
        guard result.api_version == "1", result.activation == "emacs" else {
            throw EmacsReportedStatusError.failed("Emacs 未确认目标 buffer 已激活")
        }
    }
}

private enum EmacsReportedStatusError: LocalizedError {
    case failed(String)
    var errorDescription: String? {
        switch self {
        case let .failed(message): message
        }
    }
}

@MainActor
@Observable
final class EmacsReportedStatusModel {
    private(set) var bufferGroups: [EmacsBufferGroup] = []
    private(set) var updatedAt: UInt64?
    private(set) var hasSnapshot = false
    private(set) var loading = false
    private(set) var refreshError: String?
    private(set) var activating = false
    var activationError: String?
    @ObservationIgnored private let client: EmacsReportedStatusClient
    @ObservationIgnored private let status: ServiceStatusModel
    @ObservationIgnored private let logger: CenterLog?
    @ObservationIgnored private var refreshTask: Task<Void, Never>?
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var menuOpen = false

    init(client: EmacsReportedStatusClient = .init(), status: ServiceStatusModel? = nil, logger: CenterLog? = nil) {
        self.status = status ?? client.makeStatusModel()
        self.client = client
        self.logger = logger
    }

    func menuDidOpen() {
        // 原生菜单的 tracking 事件触发一次读取，不安装轮询定时器。
        guard !menuOpen else { return }
        menuOpen = true
        refresh(operation: logger?.start("打开托盘菜单"))
    }

    func menuTrackingDidBegin() {
        menuDidOpen()
    }

    func menuDidClose() {
        menuOpen = false
        generation += 1
        refreshTask?.cancel()
        refreshTask = nil
        loading = false
    }

    func refresh() {
        refresh(operation: nil)
    }

    private func refresh(operation: CenterLogOperation?) {
        guard menuOpen, !loading else { return }
        generation += 1
        let current = generation
        loading = true
        refreshTask = Task { [weak self, status, logger] in
            do {
                await status.refresh(operation: operation)
                try Task.checkCancellation()
                guard let snapshot = status.snapshot else {
                    if case let .unavailable(message) = status.status {
                        throw EmacsReportedStatusError.failed(message)
                    }
                    throw NotificationReceiptError.serviceUnavailable
                }
                try Task.checkCancellation()
                guard let self else {
                    if let operation { logger?.finish(operation, result: "cancelled") }
                    return
                }
                guard self.menuOpen, self.generation == current else {
                    if let operation { self.logger?.finish(operation, result: "cancelled") }
                    return
                }
                self.bufferGroups = snapshot.bufferGroups
                self.updatedAt = snapshot.updatedAt
                self.hasSnapshot = true
                self.refreshError = nil
                self.loading = false
                self.refreshTask = nil
                if let operation {
                    self.logger?.finish(operation, result: "success", details: ["buffer_count": "\(snapshot.bufferGroups.reduce(0) { $0 + $1.buffers.count })"])
                }
            } catch {
                if let operation { self?.logger?.finish(operation, result: error is CancellationError ? "cancelled" : "failure", details: ["error": error.localizedDescription]) }
                guard let self, self.menuOpen, self.generation == current else { return }
                self.loading = false
                self.refreshTask = nil
                if !(error is CancellationError) { self.refreshError = error.localizedDescription }
            }
        }
    }

    func activate(_ buffer: MonitoredEmacsBuffer) {
        guard !activating else { return }
        activating = true
        let operation = logger?.start("切换 agent-shell 窗口", details: ["buffer": buffer.name])
        Task {
            defer { activating = false }
            do {
                try await client.activate(buffer, operation: operation)
                if let operation { logger?.finish(operation, result: "success") }
            }
            catch {
                let message = error.localizedDescription
                if let operation { logger?.finish(operation, result: "failure", details: ["error": message]) }
                activationError = message
                presentActivationError(message)
            }
        }
    }

    private func presentActivationError(_ message: String) {
        // 菜单点击后会立即关闭，挂在菜单内容上的 SwiftUI alert 无法稳定显示。
        // 使用独立的 AppKit modal alert，确保请求失败时用户能看到原因。
        DispatchQueue.main.async {
            let alert = NSAlert()
            alert.messageText = "无法切换 agent-shell 窗口"
            alert.informativeText = message
            alert.alertStyle = .warning
            alert.addButton(withTitle: "好")
            NSApp.activate(ignoringOtherApps: true)
            alert.runModal()
        }
    }
}

import Foundation
import Observation

struct MonitoredEmacsBuffer: Decodable, Equatable, Identifiable, Sendable {
    let id: String
    let name: String
    let mode: String
    let directory: String
    let file: String?
    let modified: Bool
}

struct EmacsBufferStatus: Decodable, Sendable {
    let apiVersion: String
    let app: String
    let key: String
    let value: [MonitoredEmacsBuffer]
    let updatedAt: UInt64?

    enum CodingKeys: String, CodingKey {
        case apiVersion = "api_version"
        case app, key, value
        case updatedAt = "updated_at"
    }
}

private struct EmacsStatusAPIError: Decodable { let message: String }
private struct EmacsBufferActivation: Decodable {
    let api_version: String
    let activation: String
    let buffer_id: String?
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

    private func endpoint() async throws -> ServiceEndpoint {
        let endpoint = try discover()
        let request = URLRequest(url: try NotificationReceiptClient.url(endpoint: endpoint, path: "/healthz"),
                                 cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 5)
        let (data, response) = try await transport(request)
        guard (response as? HTTPURLResponse)?.statusCode == 200,
              let health = try? JSONDecoder().decode(HealthResponse.self, from: data),
              health.service == "loki-services", health.status == "ok" else {
            throw NotificationReceiptError.serviceUnavailable
        }
        try Task.checkCancellation()
        return endpoint
    }

    private func send(_ request: URLRequest) async throws -> Data {
        let (data, response) = try await transport(request)
        guard let response = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        guard response.statusCode == 200 else {
            let message = (try? JSONDecoder().decode(EmacsStatusAPIError.self, from: data))?.message
            throw EmacsReportedStatusError.failed(message ?? "Emacs 状态接口返回 HTTP \(response.statusCode)")
        }
        return data
    }

    func fetchBuffers() async throws -> EmacsBufferStatus {
        let endpoint = try await endpoint()
        let request = URLRequest(url: try NotificationReceiptClient.url(endpoint: endpoint, path: "/emacs/status/emacs/buffer"),
                                 cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 5)
        let result = try JSONDecoder().decode(EmacsBufferStatus.self, from: await send(request))
        guard result.apiVersion == "1", result.app == "emacs", result.key == "buffer",
              Set(result.value.map(\.id)).count == result.value.count,
              result.value.allSatisfy({ !$0.id.isEmpty && !$0.name.isEmpty }) else {
            throw EmacsReportedStatusError.failed("Emacs buffer 状态响应无效")
        }
        return result
    }

    func activate(_ buffer: MonitoredEmacsBuffer) async throws {
        let endpoint = try await endpoint()
        let base = try NotificationReceiptClient.url(endpoint: endpoint, path: "/emacs/activate")
        var components = URLComponents(url: base, resolvingAgainstBaseURL: false)!
        components.queryItems = [URLQueryItem(name: "buffer_id", value: buffer.id)]
        var request = URLRequest(url: components.url!, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 35)
        request.httpMethod = "POST"
        let result = try JSONDecoder().decode(EmacsBufferActivation.self, from: await send(request))
        guard result.api_version == "1", result.activation == "emacs", result.buffer_id == buffer.id else {
            throw EmacsReportedStatusError.failed("Emacs 未确认目标 buffer 已激活")
        }
    }
}

private enum EmacsReportedStatusError: LocalizedError {
    case failed(String)
    var errorDescription: String? {
        switch self { case let .failed(message): message }
    }
}

@MainActor
@Observable
final class EmacsReportedStatusModel {
    private(set) var buffers: [MonitoredEmacsBuffer] = []
    private(set) var updatedAt: UInt64?
    private(set) var hasSnapshot = false
    private(set) var loading = false
    private(set) var refreshError: String?
    private(set) var activatingID: String?
    var activationError: String?
    @ObservationIgnored private let client: EmacsReportedStatusClient
    @ObservationIgnored private var refreshTask: Task<Void, Never>?
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var menuOpen = false

    init(client: EmacsReportedStatusClient = .init()) {
        self.client = client
    }

    func menuDidOpen() {
        // 原生菜单的 tracking 事件触发一次读取，不安装轮询定时器。
        guard !menuOpen else { return }
        menuOpen = true
        refresh()
    }

    func menuDidClose() {
        menuOpen = false
        generation += 1
        refreshTask?.cancel()
        refreshTask = nil
        loading = false
    }

    func refresh() {
        guard menuOpen, !loading else { return }
        generation += 1
        let current = generation
        loading = true
        refreshTask = Task { [weak self, client] in
            do {
                let snapshot = try await client.fetchBuffers()
                try Task.checkCancellation()
                guard let self, self.menuOpen, self.generation == current else { return }
                self.buffers = snapshot.value
                self.updatedAt = snapshot.updatedAt
                self.hasSnapshot = true
                self.refreshError = nil
                self.loading = false
                self.refreshTask = nil
            } catch {
                guard let self, self.menuOpen, self.generation == current else { return }
                self.loading = false
                self.refreshTask = nil
                if !(error is CancellationError) { self.refreshError = error.localizedDescription }
            }
        }
    }

    func activate(_ buffer: MonitoredEmacsBuffer) {
        guard activatingID == nil else { return }
        activatingID = buffer.id
        Task {
            defer { activatingID = nil }
            do { try await client.activate(buffer) }
            catch { activationError = error.localizedDescription }
        }
    }
}

import Foundation

/// 通过 services 的应用接口打开 Emacs 应用，Emacs 操作由服务端负责。
struct ServiceSettingsClient: Sendable {
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

    func open() async throws {
        try await open(app: "config")
    }

    func open(app: String, operation: CenterLogOperation? = nil) async throws {
        try await open(app: app, path: "/emacs/apps/open", query: [URLQueryItem(name: "app", value: app)], operation: operation)
    }

    func activateSlack(operation: CenterLogOperation? = nil) async throws {
        try await open(app: "slack", path: "/emacs/slack/activate", query: nil, operation: operation)
    }

    private func open(app: String, path: String, query: [URLQueryItem]?, operation: CenterLogOperation?) async throws {
        let endpoint = try discover()
        let healthRequest = URLRequest(
            url: try NotificationReceiptClient.url(endpoint: endpoint, path: "/healthz"),
            cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 5)
        let (healthData, healthResponse): (Data, URLResponse)
        do {
            (healthData, healthResponse) = try await transport(healthRequest)
        } catch {
            operation?.call("GET /healthz", result: "failure", details: ["error": error.localizedDescription])
            throw error
        }
        let healthCode = (healthResponse as? HTTPURLResponse)?.statusCode ?? 0
        operation?.call("GET /healthz", result: healthCode == 200 ? "success" : "failure", details: ["http_status": "\(healthCode)"])
        guard (healthResponse as? HTTPURLResponse)?.statusCode == 200,
              let health = try? JSONDecoder().decode(HealthResponse.self, from: healthData),
              health.service == "loki-services", health.status == "ok" else {
            throw NotificationReceiptError.serviceUnavailable
        }
        let openURL = try NotificationReceiptClient.url(endpoint: endpoint, path: path)
        var components = URLComponents(url: openURL, resolvingAgainstBaseURL: false)!
        components.queryItems = query
        var request = URLRequest(
            url: components.url!,
            cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 35)
        request.httpMethod = "POST"
        let (data, response): (Data, URLResponse)
        do {
            (data, response) = try await transport(request)
        } catch {
            operation?.call("POST \(path)", result: "failure", details: ["app": app, "error": error.localizedDescription])
            throw error
        }
        let statusCode = (response as? HTTPURLResponse)?.statusCode ?? 0
        operation?.call("POST \(path)", result: statusCode == 200 ? "success" : "failure", details: ["app": app, "http_status": "\(statusCode)"])
        guard let response = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        guard response.statusCode == 200 else {
            let error = try? JSONDecoder().decode(ServiceAPIErrorResponse.self, from: data)
            throw ServiceSettingsError.failed(error?.error.message ?? "打开 Emacs 应用失败（HTTP \(response.statusCode)）")
        }
        let result = try JSONDecoder().decode(SettingsOpenResponse.self, from: data)
        guard result.apiVersion == "1", result.app == app, result.opened else {
            throw ServiceSettingsError.failed("Emacs 未确认应用已打开")
        }
    }
}

private struct SettingsOpenResponse: Decodable {
    let apiVersion: String
    let app: String
    let opened: Bool

    enum CodingKeys: String, CodingKey {
        case apiVersion = "api_version"
        case app, opened
    }
}

private enum ServiceSettingsError: LocalizedError {
    case failed(String)

    var errorDescription: String? {
        switch self {
        case let .failed(message): message
        }
    }
}

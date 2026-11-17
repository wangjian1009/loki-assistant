import Foundation

/// 通过 services 的应用接口打开配置界面，Emacs 操作由服务端负责。
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
        let endpoint = try discover()
        let healthRequest = URLRequest(
            url: try NotificationReceiptClient.url(endpoint: endpoint, path: "/healthz"),
            cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 5)
        let (healthData, healthResponse) = try await transport(healthRequest)
        guard (healthResponse as? HTTPURLResponse)?.statusCode == 200,
              let health = try? JSONDecoder().decode(HealthResponse.self, from: healthData),
              health.service == "loki-services", health.status == "ok" else {
            throw NotificationReceiptError.serviceUnavailable
        }
        let openURL = try NotificationReceiptClient.url(endpoint: endpoint, path: "/emacs/apps/open")
        var components = URLComponents(url: openURL, resolvingAgainstBaseURL: false)!
        components.queryItems = [URLQueryItem(name: "app", value: "config")]
        var request = URLRequest(
            url: components.url!,
            cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 35)
        request.httpMethod = "POST"
        let (data, response) = try await transport(request)
        guard let response = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        guard response.statusCode == 200 else {
            let error = try? JSONDecoder().decode(SettingsErrorResponse.self, from: data)
            throw ServiceSettingsError.failed(error?.message ?? "打开 Emacs 配置失败（HTTP \(response.statusCode)）")
        }
        let result = try JSONDecoder().decode(SettingsOpenResponse.self, from: data)
        guard result.apiVersion == "1", result.app == "config", result.opened else {
            throw ServiceSettingsError.failed("Emacs 未确认配置界面已打开")
        }
    }
}

private struct SettingsErrorResponse: Decodable {
    let message: String
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

import Darwin
import Foundation

struct NotificationReceipt: Codable, Equatable, Sendable {
    let status: String
    let code: String?
    let message: String?

    static let submitted = NotificationReceipt(status: "submitted", code: nil, message: nil)

    static func failed(_ error: Error) -> NotificationReceipt {
        let code: String
        switch error {
        case SystemNotificationError.invalidPayload, is DecodingError: code = "invalid-payload"
        case SystemNotificationError.permissionDenied: code = "permission-denied"
        default: code = "notification-submit-failed"
        }
        return NotificationReceipt(status: "failed", code: code, message: error.localizedDescription)
    }
}

enum NotificationReceiptError: Error, LocalizedError {
    case serviceUnavailable
    case invalidEndpoint
    case rejected(Int)

    var errorDescription: String? {
        switch self {
        case .serviceUnavailable: "services 未运行或健康检查失败"
        case .invalidEndpoint: "services 回执地址无效"
        case let .rejected(status): "通知回执被 services 拒绝（HTTP \(status)）"
        }
    }
}

enum NotificationActivationError: Error, LocalizedError {
    case rejected(status: Int, message: String?)

    var errorDescription: String? {
        switch self {
        case let .rejected(status, message):
            message ?? "通知跳转失败（HTTP \(status)）"
        }
    }
}

struct NotificationReceiptClient: Sendable {
    typealias Transport = @Sendable (URLRequest) async throws -> (Data, URLResponse)
    private let discover: @Sendable () throws -> ServiceEndpoint
    private let transport: Transport
    private let retryDelay: @Sendable (Int) async throws -> Void

    init(
        discover: @escaping @Sendable () throws -> ServiceEndpoint = { try Self.discoverEndpoint() },
        transport: @escaping Transport = { try await URLSession.shared.data(for: $0) },
        retryDelay: @escaping @Sendable (Int) async throws -> Void = { try await Task.sleep(for: .seconds($0)) }
    ) {
        self.discover = discover
        self.transport = transport
        self.retryDelay = retryDelay
    }

    func send(_ receipt: NotificationReceipt, notificationID: String) async throws {
        guard UUID(uuidString: notificationID) != nil else { throw SystemNotificationError.invalidPayload }
        let body = try JSONEncoder().encode(receipt)
        for attempt in 0..<3 {
            do {
                try Task.checkCancellation()
                // 每次尝试重新发现，避免服务重启后沿用旧端口。
                let endpoint = try await healthyEndpoint()
                var request = URLRequest(url: try Self.url(endpoint: endpoint, path: "/notification/\(notificationID)/receipt"), cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 5)
                request.httpMethod = "POST"
                request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                request.httpBody = body
                let (_, response) = try await transport(request)
                guard let response = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
                guard response.statusCode == 200 else { throw NotificationReceiptError.rejected(response.statusCode) }
                return
            } catch {
                if error is CancellationError || Task.isCancelled { throw error }
                if case NotificationReceiptError.rejected(let status) = error, (400..<500).contains(status) { throw error }
                if attempt == 2 { throw error }
                try await retryDelay(attempt + 1)
            }
        }
    }

    private func healthyEndpoint() async throws -> ServiceEndpoint {
        let endpoint = try discover()
        let request = URLRequest(url: try Self.url(endpoint: endpoint, path: "/healthz"), cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 5)
        let (data, response) = try await transport(request)
        guard (response as? HTTPURLResponse)?.statusCode == 200,
              let health = try? JSONDecoder().decode(HealthResponse.self, from: data),
              health.service == "loki-services", health.status == "ok" else {
            throw NotificationReceiptError.serviceUnavailable
        }
        return endpoint
    }

    static func callbackPath(_ navigation: URL) throws -> String {
        guard let components = URLComponents(url: navigation, resolvingAgainstBaseURL: false),
              components.scheme == "loki-assistant", components.host == "notification", components.path == "/activate",
              let callback = components.queryItems?.first(where: { $0.name == "callback" })?.value
        else { throw SystemNotificationError.invalidPayload }
        try validate(callback: callback)
        return callback
    }

    private static func validate(callback: String) throws {
        guard callback.hasPrefix("/"), !callback.hasPrefix("//"), !callback.contains("\\"), !callback.contains("#"),
              let target = URLComponents(string: callback), target.scheme == nil, target.host == nil,
              !target.path.hasPrefix("//") else { throw SystemNotificationError.invalidPayload }
    }

    func activate(_ navigation: URL) async throws {
        try await activate(callback: Self.callbackPath(navigation))
    }

    func openSlackConversation(_ conversation: SlackNewConversation) async throws {
        try await activate(callback: conversation.callback, slackSessionID: conversation.sessionID)
    }

    private func activate(callback: String, slackSessionID: UUID? = nil) async throws {
        try Self.validate(callback: callback)
        let endpoint = try await healthyEndpoint()
        let base = try Self.url(endpoint: endpoint, path: "/")
        guard let target = URL(string: callback, relativeTo: base)?.absoluteURL,
              target.host == base.host, target.port == base.port, target.scheme == base.scheme else {
            throw SystemNotificationError.invalidPayload
        }
        var request = URLRequest(url: target, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 30)
        request.httpMethod = "POST"
        if let slackSessionID {
            request.setValue(slackSessionID.uuidString, forHTTPHeaderField: "X-Loki-Slack-Session")
        }
        let (data, response) = try await transport(request)
        guard let response = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        guard response.statusCode == 200 else {
            let message = (try? JSONDecoder().decode(ServiceAPIErrorResponse.self, from: data))?.error.message
            throw NotificationActivationError.rejected(status: response.statusCode, message: message?.isEmpty == false ? message : nil)
        }
    }

    static func discoverEndpoint(
        directory: URL = FileManager.default.homeDirectoryForCurrentUser.appending(path: ".assistant")
    ) throws -> ServiceEndpoint {
        let descriptor = open(directory.appending(path: "service.lock").path, O_RDONLY | O_CLOEXEC)
        guard descriptor >= 0 else { throw NotificationReceiptError.serviceUnavailable }
        defer { close(descriptor) }
        if flock(descriptor, LOCK_EX | LOCK_NB) == 0 {
            flock(descriptor, LOCK_UN)
            throw NotificationReceiptError.serviceUnavailable
        }
        guard errno == EWOULDBLOCK else { throw NotificationReceiptError.serviceUnavailable }
        let data = try Data(contentsOf: directory.appending(path: "service"))
        let endpoint = try JSONDecoder().decode(ServiceEndpoint.self, from: data)
        _ = try url(endpoint: endpoint, path: "/healthz")
        return endpoint
    }

    static func url(endpoint: ServiceEndpoint, path: String) throws -> URL {
        var ipv4 = in_addr()
        var ipv6 = in6_addr()
        let loopback: Bool
        if inet_pton(AF_INET, endpoint.host, &ipv4) == 1 {
            loopback = UInt32(bigEndian: ipv4.s_addr) >> 24 == 127
        } else if inet_pton(AF_INET6, endpoint.host, &ipv6) == 1 {
            var expected = in6addr_loopback
            loopback = memcmp(&ipv6, &expected, MemoryLayout<in6_addr>.size) == 0
        } else {
            loopback = false
        }
        guard loopback, endpoint.port != 0 else { throw NotificationReceiptError.invalidEndpoint }
        var components = URLComponents()
        components.scheme = "http"
        components.host = endpoint.host.contains(":") ? "[\(endpoint.host)]" : endpoint.host
        components.port = Int(endpoint.port)
        components.path = path
        guard let url = components.url else { throw NotificationReceiptError.invalidEndpoint }
        return url
    }
}

/// 展示仅执行一次；网络重试完全由回执客户端负责。
@MainActor
struct NotificationDelivery {
    let present: (SystemNotification) async throws -> Void
    let report: (String, NotificationReceipt) async throws -> Void

    func handle(_ url: URL) async throws {
        let id = try SystemNotification.receiptID(url: url)
        let receipt: NotificationReceipt
        do {
            try await present(SystemNotification(url: url))
            receipt = .submitted
        } catch {
            guard id != nil else { throw error }
            receipt = .failed(error)
        }
        if let id { try await report(id, receipt) }
    }
}

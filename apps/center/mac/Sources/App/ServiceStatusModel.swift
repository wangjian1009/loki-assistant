import Foundation
import Observation

struct HealthResponse: Codable, Equatable, Sendable {
    let service: String
    let status: String
    let version: String
    let uptimeSeconds: Int

    enum CodingKeys: String, CodingKey {
        case service
        case status
        case version
        case uptimeSeconds = "uptime_seconds"
    }
}

struct ServiceEndpoint: Codable, Equatable, Sendable {
    let host: String
    let port: UInt16
    let pid: UInt32
    let version: String
    let startedAt: UInt64

    enum CodingKeys: String, CodingKey {
        case host, port, pid, version
        case startedAt = "started_at"
    }
}

struct SelfServiceStatusModule: Decodable, Equatable, Sendable {
    let startedAt: UInt64

    enum CodingKeys: String, CodingKey {
        case startedAt = "start_at"
    }
}

struct NetworkStatusValue: Decodable, Equatable, Sendable {
    let status: String
}

struct NetworkStatusModule: Decodable, Equatable, Sendable {
    let my: NetworkStatusValue?
}

struct ServiceStatusModules: Decodable, Equatable, Sendable {
    let service: SelfServiceStatusModule?
    let network: NetworkStatusModule?
}

struct ServiceStatusSnapshot: Decodable, Equatable, Sendable {
    let apiVersion: String
    let status: ServiceStatusModules
    let updatedAt: UInt64?
    let bufferGroups: [EmacsBufferGroup]

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        apiVersion = try container.decode(String.self, forKey: .apiVersion)
        status = try container.decode(ServiceStatusModules.self, forKey: .status)
        updatedAt = try container.decodeIfPresent(UInt64.self, forKey: .updatedAt)
        bufferGroups = try EmacsBufferStatus(from: decoder).bufferGroups
    }

    enum CodingKeys: String, CodingKey {
        case apiVersion = "api_version"
        case status
        case updatedAt = "updated_at"
    }
}

enum ServiceStatus: Equatable, Sendable {
    case unknown
    case healthy(String)
    case stopped
    case unavailable(String)
}

enum ServiceIndicator: Hashable {
    case red, yellow, green
}

@MainActor
@Observable
final class ServiceStatusModel {
    typealias Discover = @Sendable () throws -> ServiceEndpoint
    typealias Transport = @Sendable (URLRequest) async throws -> (Data, URLResponse)

    private(set) var status: ServiceStatus = .unknown
    private(set) var lastUpdated: Date?
    private(set) var startedAt: Date?
    private(set) var networkStatus: String?
    private(set) var snapshot: ServiceStatusSnapshot?

    var indicator: ServiceIndicator {
        switch status {
        case .stopped, .unavailable: .red
        case .unknown: .yellow
        case .healthy:
            switch networkStatus {
            case "offline", "connected": .green
            default: .yellow
            }
        }
    }
    @ObservationIgnored private let discover: Discover
    @ObservationIgnored private let transport: Transport

    init(
        discover: @escaping Discover = { try NotificationReceiptClient.discoverEndpoint() },
        transport: @escaping Transport = { try await URLSession.shared.data(for: $0) }
    ) {
        self.discover = discover
        self.transport = transport
    }

    func refresh(operation: CenterLogOperation? = nil) async {
        do {
            // 与通知回执共用文件锁和 pfile 发现，不依赖任何服务管理器的任务名。
            let endpoint = try discover()
            let request = URLRequest(
                url: try NotificationReceiptClient.url(endpoint: endpoint, path: "/status"),
                cachePolicy: .reloadIgnoringLocalCacheData,
                timeoutInterval: 5
            )
            let (data, response): (Data, URLResponse)
            do {
                (data, response) = try await transport(request)
            } catch {
                operation?.call("GET /status", result: "failure", details: ["error": error.localizedDescription])
                throw error
            }
            try Task.checkCancellation()
            let statusCode = (response as? HTTPURLResponse)?.statusCode ?? 0
            operation?.call("GET /status", result: statusCode == 200 ? "success" : "failure", details: ["http_status": "\(statusCode)"])
            if statusCode != 200,
               let message = (try? JSONDecoder().decode(ServiceAPIErrorResponse.self, from: data))?.error.message {
                throw NSError(domain: "LokiAssistant.Status", code: statusCode, userInfo: [NSLocalizedDescriptionKey: message])
            }
            guard (response as? HTTPURLResponse)?.statusCode == 200 else {
                throw URLError(.badServerResponse)
            }
            let snapshot = try JSONDecoder().decode(ServiceStatusSnapshot.self, from: data)
            guard snapshot.apiVersion == "1" else {
                throw URLError(.cannotParseResponse)
            }
            status = .healthy(endpoint.version)
            self.snapshot = snapshot
            startedAt = snapshot.status.service.map {
                Date(timeIntervalSince1970: TimeInterval($0.startedAt))
            }
            networkStatus = snapshot.status.network?.my?.status
        } catch is CancellationError {
            return
        } catch NotificationReceiptError.serviceUnavailable {
            status = .stopped
            startedAt = nil
            networkStatus = nil
            snapshot = nil
        } catch {
            guard !Task.isCancelled else { return }
            status = .unavailable(error.localizedDescription)
            startedAt = nil
            networkStatus = nil
            snapshot = nil
        }
        lastUpdated = Date()
    }
}

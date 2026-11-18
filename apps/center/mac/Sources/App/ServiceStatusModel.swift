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

enum ServiceStatus: Equatable, Sendable {
    case unknown
    case healthy(HealthResponse)
    case stopped
    case unavailable(String)
}

@MainActor
@Observable
final class ServiceStatusModel {
    typealias Discover = @Sendable () throws -> ServiceEndpoint
    typealias Transport = @Sendable (URLRequest) async throws -> (Data, URLResponse)

    private(set) var status: ServiceStatus = .unknown
    private(set) var lastUpdated: Date?
    private(set) var startedAt: Date?
    @ObservationIgnored private var monitoringTask: Task<Void, Never>?
    @ObservationIgnored private let discover: Discover
    @ObservationIgnored private let transport: Transport

    init(
        discover: @escaping Discover = { try NotificationReceiptClient.discoverEndpoint() },
        transport: @escaping Transport = { try await URLSession.shared.data(for: $0) }
    ) {
        self.discover = discover
        self.transport = transport
    }

    func startMonitoring() {
        guard monitoringTask == nil else { return }
        monitoringTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(for: .seconds(5))
            }
        }
    }

    func refresh() async {
        do {
            // 与通知回执共用文件锁和 pfile 发现，不依赖任何服务管理器的任务名。
            let endpoint = try discover()
            let request = URLRequest(
                url: try NotificationReceiptClient.url(endpoint: endpoint, path: "/healthz"),
                cachePolicy: .reloadIgnoringLocalCacheData,
                timeoutInterval: 5
            )
            let (data, response) = try await transport(request)
            guard (response as? HTTPURLResponse)?.statusCode == 200 else {
                throw URLError(.badServerResponse)
            }
            let health = try JSONDecoder().decode(HealthResponse.self, from: data)
            guard health.service == "loki-services", health.status == "ok" else {
                throw URLError(.cannotParseResponse)
            }
            status = .healthy(health)
            startedAt = Date(timeIntervalSince1970: TimeInterval(endpoint.startedAt))
        } catch NotificationReceiptError.serviceUnavailable {
            status = .stopped
            startedAt = nil
        } catch {
            status = .unavailable(error.localizedDescription)
            startedAt = nil
        }
        lastUpdated = Date()
    }
}

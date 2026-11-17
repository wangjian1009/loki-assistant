import Darwin
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
    case starting
    case healthy(HealthResponse)
    case stopped
    case unavailable(String)
}

private struct LaunchctlResult: Sendable {
    let status: Int32
    let output: String
}

@MainActor
@Observable
final class ServiceStatusModel {
    nonisolated static let serviceFileURL = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: ".assistant/service")
    nonisolated static let serviceLabel = "cc.loki-assistant.services"
    nonisolated static let launchctlURL = URL(fileURLWithPath: "/bin/launchctl")

    private(set) var status: ServiceStatus = .unknown
    private(set) var lastUpdated: Date?
    private(set) var startedAt: Date?
    @ObservationIgnored private var monitoringTask: Task<Void, Never>?

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
            let printResult = try await runLaunchctl(["print", serviceTarget])
            guard printResult.status == 0 else {
                status = .stopped
                startedAt = nil
                lastUpdated = Date()
                return
            }
            let endpoint = try readEndpoint()
            let health = try await fetchHealth(endpoint: endpoint)
            status = .healthy(health)
            startedAt = Date(timeIntervalSince1970: TimeInterval(endpoint.startedAt))
            lastUpdated = Date()
        } catch {
            status = .unavailable(error.localizedDescription)
            startedAt = nil
            lastUpdated = Date()
        }
    }

    func start() async {
        status = .starting
        do {
            let plistURL = Self.plistURL
            guard FileManager.default.fileExists(atPath: plistURL.path) else {
                throw ServiceError.notRegistered
            }
            let printResult = try await runLaunchctl(["print", serviceTarget])
            if printResult.status == 0 {
                _ = try await runLaunchctl(["kickstart", "-k", serviceTarget])
            } else {
                _ = try await runLaunchctl(["bootstrap", serviceDomain, plistURL.path])
            }
            await refresh()
        } catch {
            status = .unavailable(error.localizedDescription)
            lastUpdated = Date()
        }
    }

    private var serviceDomain: String {
        "gui/\(getuid())"
    }

    private var serviceTarget: String {
        "\(serviceDomain)/\(Self.serviceLabel)"
    }

    private static var plistURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/LaunchAgents")
            .appending(path: "\(serviceLabel).plist")
    }

    private func fetchHealth(endpoint: ServiceEndpoint) async throws -> HealthResponse {
        guard let url = URL(string: "http://\(endpoint.host):\(endpoint.port)/healthz") else {
            throw URLError(.badURL)
        }
        let (data, response) = try await URLSession.shared.data(from: url)
        guard let httpResponse = response as? HTTPURLResponse,
              httpResponse.statusCode == 200 else {
            throw URLError(.badServerResponse)
        }
        let health = try JSONDecoder().decode(HealthResponse.self, from: data)
        guard health.status == "ok" else {
            throw URLError(.cannotParseResponse)
        }
        return health
    }

    private func readEndpoint() throws -> ServiceEndpoint {
        let data = try Data(contentsOf: Self.serviceFileURL)
        return try JSONDecoder().decode(ServiceEndpoint.self, from: data)
    }

    private func runLaunchctl(_ arguments: [String]) async throws -> LaunchctlResult {
        try await withCheckedThrowingContinuation { continuation in
            let process = Process()
            let outputPipe = Pipe()
            let errorPipe = Pipe()
            process.executableURL = Self.launchctlURL
            process.arguments = arguments
            process.standardOutput = outputPipe
            process.standardError = errorPipe
            process.terminationHandler = { process in
                let output = outputPipe.fileHandleForReading.readDataToEndOfFile()
                let error = errorPipe.fileHandleForReading.readDataToEndOfFile()
                let text = String(data: error.isEmpty ? output : error, encoding: .utf8) ?? ""
                continuation.resume(returning: LaunchctlResult(status: process.terminationStatus, output: text))
            }
            do {
                try process.run()
            } catch {
                continuation.resume(throwing: error)
            }
        }
    }
}

private enum ServiceError: LocalizedError {
    case notRegistered

    var errorDescription: String? {
        switch self {
        case .notRegistered:
            "services 尚未注册 LaunchAgent"
        }
    }
}

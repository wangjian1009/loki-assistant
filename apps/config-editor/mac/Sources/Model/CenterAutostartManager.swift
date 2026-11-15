import Darwin
import Foundation

struct CenterLaunchctlResult: Sendable {
    let status: Int32
    let output: String
}

enum CenterAutostartManager {
    static let label = "cc.loki-assistant.center"
    typealias Runner = @Sendable ([String]) async throws -> CenterLaunchctlResult

    static func setEnabled(_ enabled: Bool) async throws {
        let homeDirectory = FileManager.default.homeDirectoryForCurrentUser
        let executable = Bundle.main.bundleURL
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appending(path: "center/mac/.build/Debug/LokiAssistant.app/Contents/MacOS/LokiAssistant")
        try await apply(enabled,
                        plistURL: homeDirectory.appending(path: "Library/LaunchAgents/\(label).plist"),
                        executableURL: executable,
                        logDirectory: homeDirectory.appending(path: "Library/Logs/LokiAssistant"),
                        domain: "gui/\(getuid())", run: run)
    }

    static func apply(_ enabled: Bool, plistURL: URL, executableURL: URL, logDirectory: URL,
                      domain: String, run: Runner) async throws {
        let target = "\(domain)/\(label)"
        if enabled {
            guard FileManager.default.isExecutableFile(atPath: executableURL.path) else {
                throw AutostartError.failed("托盘程序不存在或不可执行，请先构建托盘程序")
            }
            let plist: [String: Any] = [
                "Label": label,
                "ProgramArguments": [executableURL.path],
                "RunAtLoad": true,
                "KeepAlive": false,
                "LimitLoadToSessionType": "Aqua",
                "StandardOutPath": logDirectory.appending(path: "center.log").path,
                "StandardErrorPath": logDirectory.appending(path: "center.error.log").path
            ]
            let data = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
            try FileManager.default.createDirectory(at: plistURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            try FileManager.default.createDirectory(at: logDirectory, withIntermediateDirectories: true)
            try data.write(to: plistURL, options: .atomic)
            try await checked(["enable", target], run: run)
            let registered = try await run(["print", target])
            if registered.status == 0 {
                // 不使用 -k，已运行的托盘实例继续运行，停止的已注册任务则启动。
                try await checked(["kickstart", target], run: run)
            } else {
                try await checked(["bootstrap", domain, plistURL.path], run: run)
            }
        } else {
            let registered = try await run(["print", target])
            if registered.status == 0 {
                try await checked(["bootout", target], run: run)
            }
            if FileManager.default.fileExists(atPath: plistURL.path) {
                try FileManager.default.removeItem(at: plistURL)
            }
        }
    }

    private static func checked(_ arguments: [String], run: Runner) async throws {
        let result = try await run(arguments)
        guard result.status == 0 else {
            throw AutostartError.failed("launchctl \(arguments.joined(separator: " "))：\(result.output)")
        }
    }

    private static func run(_ arguments: [String]) async throws -> CenterLaunchctlResult {
        try await withCheckedThrowingContinuation { continuation in
            let process = Process()
            let output = Pipe()
            process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
            process.arguments = arguments
            process.standardOutput = output
            process.standardError = output
            process.terminationHandler = { process in
                let data = output.fileHandleForReading.readDataToEndOfFile()
                continuation.resume(returning: CenterLaunchctlResult(
                    status: process.terminationStatus,
                    output: String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                ))
            }
            do { try process.run() }
            catch { continuation.resume(throwing: error) }
        }
    }

    private enum AutostartError: LocalizedError {
        case failed(String)
        var errorDescription: String? {
            switch self {
            case let .failed(message): "无法更新托盘自动启动：\(message)"
            }
        }
    }
}

import Foundation
import OSLog

/// 在独立队列写入托盘日志，一次提交完整的操作及其调用。
final class CenterLog: Sendable {
    static let shared = CenterLog()
    static let defaultFileURL = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: ".assistant/logs/center.log")

    private let fileURL: URL
    private let queue = DispatchQueue(label: "cc.loki-assistant.center.log")
    private let fallback = Logger(subsystem: "cc.loki-assistant.center", category: "file-log")
    private let maximumEntries = 500

    init(fileURL: URL = CenterLog.defaultFileURL) {
        self.fileURL = fileURL
    }

    func start(_ action: String, details: [String: String] = [:], date: Date = Date()) -> CenterLogOperation {
        CenterLogOperation(action: action, details: details, startedAt: date)
    }

    func finish(_ operation: CenterLogOperation, result: String, details: [String: String] = [:], date: Date = Date()) {
        guard let entry = operation.finish(result: result, details: details, endedAt: date) else { return }
        enqueue(entry)
    }

    func record(_ message: String, level: String = "INFO", date: Date = Date()) {
        enqueue(Self.heading(message, level: level, date: date))
    }

    func flush() {
        queue.sync {}
    }

    private func enqueue(_ entry: String) {
        queue.async { [self] in
            do {
                try write(entry)
            } catch {
                fallback.error("写入托盘日志文件失败：\(error.localizedDescription, privacy: .public)")
            }
        }
    }

    private func write(_ entry: String) throws {
        let manager = FileManager.default
        let directory = fileURL.deletingLastPathComponent()
        try manager.createDirectory(at: directory, withIntermediateDirectories: true,
                                    attributes: [.posixPermissions: 0o700])
        let existing: String
        do {
            existing = try String(contentsOf: fileURL, encoding: .utf8)
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            existing = ""
        }
        var entries: [String] = []
        for line in existing.split(separator: "\n") {
            if line.hasPrefix(" "), !entries.isEmpty {
                entries[entries.count - 1] += "\n\(line)"
            } else {
                entries.append(String(line))
            }
        }
        entries.append(entry)
        let content = entries.suffix(maximumEntries).joined(separator: "\n") + "\n"
        let temporary = directory.appending(path: ".center.\(UUID().uuidString).tmp")
        let descriptor = Darwin.open(temporary.path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0o600)
        guard descriptor >= 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
        defer { try? manager.removeItem(at: temporary) }
        let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
        try handle.write(contentsOf: Data(content.utf8))
        try handle.close()
        guard Darwin.rename(temporary.path, fileURL.path) == 0 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
    }

    fileprivate static func heading(_ message: String, level: String, date: Date) -> String {
        return "\(timestamp(date)) [center] \(level) center - \(safe(message))"
    }

    fileprivate static func timestamp(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss.SSS"
        return formatter.string(from: date)
    }

    fileprivate static func safe(_ value: String) -> String {
        value.components(separatedBy: CharacterSet.newlines.union(.controlCharacters))
            .joined(separator: " ")
            .trimmingCharacters(in: .whitespaces)
    }

    fileprivate static func fields(_ details: [String: String]) -> [String] {
        details.keys.sorted().map { key in
            "\(safe(key))=\(safe(details[key] ?? ""))"
        }
    }
}

final class CenterLogOperation: @unchecked Sendable {
    private struct Call {
        let action: String
        let result: String
        let details: [String: String]
        let timestamp: Date
    }

    private let action: String
    private let details: [String: String]
    private let startedAt: Date
    private let lock = NSLock()
    private var calls: [Call] = []
    private var completed = false

    fileprivate init(action: String, details: [String: String], startedAt: Date) {
        self.action = action
        self.details = details
        self.startedAt = startedAt
    }

    func call(_ action: String, result: String, details: [String: String] = [:], date: Date = Date()) {
        lock.lock()
        defer { lock.unlock() }
        guard !completed else { return }
        calls.append(Call(action: action, result: result, details: details, timestamp: date))
    }

    fileprivate func finish(result: String, details: [String: String], endedAt: Date) -> String? {
        lock.lock()
        defer { lock.unlock() }
        guard !completed else { return nil }
        completed = true
        let heading = CenterLog.heading(action, level: result == "failure" ? "ERROR" : "INFO", date: startedAt)
        var metadata = self.details.merging(details) { _, new in new }
        metadata["result"] = result
        metadata["duration_ms"] = String(Int(max(0, endedAt.timeIntervalSince(startedAt)) * 1000))
        var lines = [heading, "  " + CenterLog.fields(metadata).joined(separator: " ")]
        for call in calls {
            var fields = call.details
            fields["result"] = call.result
            let suffix = CenterLog.fields(fields).joined(separator: " ")
            lines.append("  \(CenterLog.timestamp(call.timestamp)) \(CenterLog.safe(call.action))  \(suffix)")
        }
        return lines.joined(separator: "\n")
    }
}

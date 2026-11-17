import Foundation

/// 托盘用户操作日志；文件使用 Org 结构，一级标题是用户操作，二级标题是本次操作中的调用。
final class CenterLog: @unchecked Sendable {
    static let shared = CenterLog()

    static let defaultFileURL = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: ".assistant/loki-assistant.log.org")

    private let fileURL: URL
    private let lock = NSLock()
    private let retention: TimeInterval = 3 * 24 * 60 * 60

    init(fileURL: URL = CenterLog.defaultFileURL) {
        self.fileURL = fileURL
    }

    func start(_ action: String, details: [String: String] = [:], date: Date = Date()) -> CenterLogOperation {
        CenterLogOperation(action: action, details: details, startedAt: date)
    }

    func finish(_ operation: CenterLogOperation, result: String, details: [String: String] = [:], date: Date = Date()) {
        let entry = operation.finish(result: result, details: details, endedAt: date)
        guard let entry else { return }
        lock.lock()
        defer { lock.unlock() }
        do {
            try FileManager.default.createDirectory(
                at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            let existing = (try? String(contentsOf: fileURL, encoding: .utf8)) ?? Self.header
            let cutoff = date.addingTimeInterval(-retention)
            let retained = Self.entries(in: existing).filter { $0.timestamp >= cutoff }
            let content = Self.render(header: existing, entries: retained + [entry])
            try content.write(to: fileURL, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fileURL.path)
        } catch {
            // 日志不能阻断托盘操作；文件错误留给下一次写入重试。
        }
    }

    private static let header = """
#+TITLE: Loki Assistant 托盘日志
#+STARTUP: overview
#+OPTIONS: toc:nil

"""

    fileprivate struct Entry {
        let timestamp: Date
        let text: String
    }

    private static func entries(in content: String) -> [Entry] {
        var entries: [Entry] = []
        var current: [String] = []
        for line in content.components(separatedBy: .newlines) {
            if line.hasPrefix("* ") {
                if let entry = entry(from: current) { entries.append(entry) }
                current = [line]
            } else if !current.isEmpty {
                current.append(line)
            }
        }
        if let entry = entry(from: current) { entries.append(entry) }
        return entries
    }

    private static func entry(from lines: [String]) -> Entry? {
        guard !lines.isEmpty,
              let timestampLine = lines.first(where: { $0.hasPrefix(":TIMESTAMP:") }),
              let value = timestampLine.split(separator: ":", maxSplits: 2).last,
              let timestamp = isoDate(from: value.trimmingCharacters(in: .whitespaces)) else {
            return nil
        }
        return Entry(timestamp: timestamp, text: lines.joined(separator: "\n"))
    }

    private static func render(header existing: String, entries: [Entry]) -> String {
        let headerText = existing.range(of: "\n* ", options: .literal).map { String(existing[..<$0.lowerBound]) } ?? header
        return headerText.trimmingCharacters(in: .whitespacesAndNewlines)
            + "\n\n"
            + entries.map(\.text).joined(separator: "\n\n")
            + "\n"
    }

    fileprivate static func isoString(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withColonSeparatorInTime]
        return formatter.string(from: date)
    }

    fileprivate static func isoDate(from value: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withColonSeparatorInTime]
        return formatter.date(from: value)
    }

    fileprivate static func headingString(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd EEE HH:mm"
        return formatter.string(from: date)
    }

    fileprivate static func safe(_ value: String) -> String {
        value.replacingOccurrences(of: "\r", with: " ")
            .replacingOccurrences(of: "\n", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

final class CenterLogOperation: @unchecked Sendable {
    fileprivate struct Call: Sendable {
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
        calls.append(Call(action: action, result: result, details: details, timestamp: date))
        lock.unlock()
    }

    fileprivate func finish(result: String, details: [String: String], endedAt: Date) -> CenterLog.Entry? {
        lock.lock()
        defer { lock.unlock() }
        guard !completed else { return nil }
        completed = true
        let title = CenterLog.safe(action)
        let timestamp = CenterLog.isoString(startedAt)
        let heading = CenterLog.headingString(startedAt)
        var lines = ["* [\(heading)] \(title)", ":PROPERTIES:", ":TIMESTAMP: \(timestamp)", ":ENDED_AT: \(CenterLog.isoString(endedAt))", ":RESULT: \(CenterLog.safe(result))"]
        var allDetails = self.details
        details.forEach { allDetails[$0.key] = $0.value }
        lines.append(contentsOf: properties(allDetails))
        lines.append(":END:")
        lines.append(contentsOf: calls.map(renderCall))
        return CenterLog.Entry(timestamp: endedAt, text: lines.joined(separator: "\n"))
    }

    private func properties(_ values: [String: String]) -> [String] {
        values.keys.sorted().map { key in
            ":\(CenterLog.safe(key).uppercased()): \(CenterLog.safe(values[key] ?? ""))"
        }
    }

    private func renderCall(_ call: Call) -> String {
        var lines = ["** [\(CenterLog.headingString(call.timestamp))] \(CenterLog.safe(call.action))", ":PROPERTIES:", ":TIMESTAMP: \(CenterLog.isoString(call.timestamp))", ":RESULT: \(CenterLog.safe(call.result))"]
        lines.append(contentsOf: properties(call.details))
        lines.append(":END:")
        return lines.joined(separator: "\n")
    }
}

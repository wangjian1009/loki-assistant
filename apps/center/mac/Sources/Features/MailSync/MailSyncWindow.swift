import AppKit
import SwiftUI

@MainActor
final class MailSyncWindow: NSObject, NSWindowDelegate {
    private var window: NSWindow?
    private var syncTask: Task<Void, Never>?

    func syncFromMenu() {
        if let window, syncTask != nil {
            if window.isMiniaturized {
                window.deminiaturize(nil)
            }
            window.makeKeyAndOrderFront(nil)
            NSApp.activate()
            return
        }

        let window = self.window ?? NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 324),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        let sync = MailSyncSession()
        window.title = "邮件同步"
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.contentViewController = NSHostingController(rootView: MailSyncView(session: sync))
        window.contentMinSize = NSSize(width: 400, height: 280)
        if self.window == nil {
            window.setContentSize(NSSize(width: 520, height: 324))
            window.center()
        }
        self.window = window
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
        syncTask = Task { [weak self] in
            guard !Task.isCancelled else { return }
            await sync.run()
            if !Task.isCancelled {
                self?.syncTask = nil
            }
        }
    }

    func windowWillClose(_ notification: Notification) {
        syncTask?.cancel()
        syncTask = nil
        window = nil
    }
}

private struct MailSyncView: View {
    let session: MailSyncSession

    var body: some View {
        let snapshot = session.snapshot
        let connectionError = session.connectionError
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 10) {
                if connectionError != nil || snapshot?.phase == .failed {
                    Image(systemName: "exclamationmark.circle.fill")
                        .foregroundStyle(.red)
                        .accessibilityHidden(true)
                } else if snapshot?.phase == .completed {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                        .accessibilityHidden(true)
                } else {
                    ProgressView().controlSize(.small).accessibilityHidden(true)
                }
                Text(connectionError == nil ? (snapshot?.phase.title ?? "正在连接 services…") : "无法获取同步进度")
                    .font(.title3.weight(.semibold))
            }

            if let steps = snapshot?.phase.finishedSteps, connectionError == nil {
                VStack(alignment: .leading, spacing: 6) {
                    ProgressView(value: steps, total: 3)
                        .accessibilityLabel("同步阶段")
                    HStack {
                        Text("下载邮件")
                        Spacer()
                        Text("更新索引")
                        Spacer()
                        Text("刷新未读")
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                }
            }

            if let error = connectionError ?? snapshot?.error {
                ViewThatFits(in: .vertical) {
                    Text(error).fixedSize(horizontal: false, vertical: true)
                    ScrollView {
                        Text(error).frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .frame(maxHeight: 72, alignment: .topLeading)
                .foregroundStyle(.red)
                .font(.callout)
                .textSelection(.enabled)
            }

            VStack(alignment: .leading, spacing: 8) {
                Text("同步输出")
                    .font(.subheadline.weight(.semibold))
                let messages = snapshot?.messages ?? []
                ScrollViewReader { scroll in
                    ScrollView {
                        VStack(alignment: .leading) {
                            if messages.isEmpty {
                                Text(snapshot?.phase.outputPlaceholder ?? "正在连接 services…")
                                    .foregroundStyle(.secondary)
                            } else {
                                Text(verbatim: messages.joined(separator: "\n"))
                                    .font(.system(.callout, design: .monospaced))
                                    .textSelection(.enabled)
                            }
                            Color.clear.frame(height: 0).id("end")
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(12)
                    }
                    .onChange(of: messages) {
                        scroll.scrollTo("end", anchor: .bottom)
                    }
                }
                .frame(minHeight: 72, maxHeight: .infinity)
                .background(.background, in: RoundedRectangle(cornerRadius: 8))
                .overlay {
                    RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary)
                }
            }
        }
        .padding(20)
        .frame(minWidth: 400, minHeight: 280)
    }
}

@MainActor
@Observable
private final class MailSyncSession {
    private(set) var snapshot: MailSyncSnapshot?
    private(set) var connectionError: String?

    func run() async {
        let operation = CenterLog.shared.start("立即同步邮件")
        do {
            let client = MailSyncClient()
            let endpoint = try await NotificationReceiptClient().healthyEndpoint()
            try Task.checkCancellation()
            let final = try await client.monitor(at: endpoint) { update in
                if snapshot == nil {
                    operation.call("POST /mail/sync", result: "accepted")
                }
                snapshot = update
            }
            CenterLog.shared.finish(operation, result: final.phase == .completed ? "success" : "failure")
        } catch {
            if !Task.isCancelled {
                connectionError = error.localizedDescription
                CenterLog.shared.finish(operation, result: "failure")
            }
        }
        if Task.isCancelled {
            CenterLog.shared.finish(operation, result: "cancelled")
        }
    }
}

struct MailSyncSnapshot: Decodable {
    let id: UUID
    let phase: Phase
    let messages: [String]
    let error: String?

    enum Phase: String, Decodable {
        case queued, downloading, indexing, refreshing, completed, failed

        var isTerminal: Bool { self == .completed || self == .failed }

        var title: String {
            switch self {
            case .queued: "正在等待同步"
            case .downloading: "正在同步下载邮件"
            case .indexing: "正在更新邮件索引"
            case .refreshing: "正在刷新未读邮件"
            case .completed: "邮件同步完成"
            case .failed: "邮件同步失败"
            }
        }

        var finishedSteps: Double? {
            switch self {
            case .queued, .downloading: 0
            case .indexing: 1
            case .refreshing: 2
            case .completed: 3
            case .failed: nil
            }
        }

        var outputPlaceholder: String {
            switch self {
            case .queued: "同步开始后将显示输出"
            case .downloading: "正在等待 mbsync 输出"
            case .indexing: "正在等待 mu 输出"
            case .refreshing, .completed, .failed: "暂无同步输出"
            }
        }
    }
}

/// 服务端将每个完整快照编码为一行 SSE data，心跳与事件名称不携带快照。
struct MailSyncEventDecoder {
    private struct ProgressResponse: Decodable {
        let api_version: String
        let sync: MailSyncSnapshot
    }

    private var id: UUID?

    mutating func decode(_ line: String) throws -> MailSyncSnapshot? {
        guard line.hasPrefix("data:") else { return nil }
        let payload = line.dropFirst("data:".count)
        let result = try JSONDecoder().decode(ProgressResponse.self, from: Data(payload.utf8))
        guard result.api_version == "1", id == nil || id == result.sync.id else {
            throw MailSyncMonitorError(message: "服务返回了无效的邮件同步进度")
        }
        id = result.sync.id
        return result.sync
    }
}

private struct MailSyncMonitorError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

@MainActor
private struct MailSyncClient {
    func monitor(
        at endpoint: ServiceEndpoint, onUpdate: (MailSyncSnapshot) -> Void
    ) async throws -> MailSyncSnapshot {
        let url = try NotificationReceiptClient.url(endpoint: endpoint, path: "/mail/sync")
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 40)
        request.httpMethod = "POST"
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        let (bytes, response) = try await URLSession.shared.bytes(for: request)
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        guard http.statusCode == 202 else {
            var data = Data()
            for try await byte in bytes {
                data.append(byte)
                if data.count >= 16_384 { break }
            }
            let error = try? JSONDecoder().decode(ServiceAPIErrorResponse.self, from: data)
            throw MailSyncMonitorError(message: error?.error.message ?? "邮件同步请求失败（HTTP \(http.statusCode)）")
        }
        guard http.mimeType == "text/event-stream" else {
            throw MailSyncMonitorError(message: "服务未返回邮件同步进度流")
        }
        var decoder = MailSyncEventDecoder()
        for try await line in bytes.lines {
            try Task.checkCancellation()
            guard let update = try decoder.decode(line) else { continue }
            onUpdate(update)
            if update.phase.isTerminal { return update }
        }
        throw MailSyncMonitorError(message: "进度连接已断开，未收到同步结果")
    }
}

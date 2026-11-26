import AppKit
import SwiftUI

@MainActor
final class MailSyncWindow: NSObject, NSWindowDelegate {
    private var window: NSWindow?
    private var tracking = false

    func show() {
        if let window, tracking {
            if window.isMiniaturized {
                window.deminiaturize(nil)
            }
            window.makeKeyAndOrderFront(nil)
            NSApp.activate()
            return
        }

        let window = self.window ?? NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 300),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        tracking = true
        window.title = "邮件同步"
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.contentViewController = NSHostingController(rootView: MailSyncView {
            [weak self] in self?.tracking = false
        })
        window.contentMinSize = NSSize(width: 380, height: 240)
        if self.window == nil {
            window.setContentSize(NSSize(width: 520, height: 300))
            window.center()
        }
        self.window = window
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
    }

    func windowWillClose(_ notification: Notification) {
        // 移除 SwiftUI 视图会结束进度查询，services 继续当前同步。
        window?.contentViewController = nil
        window = nil
        tracking = false
    }
}

private struct MailSyncView: View {
    let finished: @MainActor () -> Void
    @State private var snapshot: MailSyncSnapshot?
    @State private var connectionError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(connectionError == nil ? (snapshot?.phase.title ?? "正在连接 services…") : "无法获取同步进度")
                .font(.headline)

            if let steps = snapshot?.phase.finishedSteps {
                ProgressView(value: steps, total: 3)
                    .accessibilityLabel("同步阶段")
            } else if snapshot == nil && connectionError == nil {
                ProgressView()
                    .progressViewStyle(.linear)
                    .accessibilityLabel("正在连接 services")
            }

            if let error = connectionError ?? snapshot?.error {
                Label(error, systemImage: "exclamationmark.circle")
                    .foregroundStyle(.red)
                    .font(.callout)
                    .textSelection(.enabled)
            }

            Text("mu 索引消息")
                .font(.subheadline.weight(.semibold))

            let messages = snapshot?.messages ?? []
            ScrollViewReader { scroll in
                ScrollView {
                    VStack(alignment: .leading) {
                        if messages.isEmpty {
                            Text("暂无索引消息")
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
            .background(.background, in: RoundedRectangle(cornerRadius: 8))
            .overlay {
                RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary)
            }
        }
        .padding(20)
        .frame(minWidth: 380, minHeight: 240)
        .task { await sync() }
    }

    private func sync() async {
        let operation = CenterLog.shared.start("立即同步邮件")
        do {
            let client = MailSyncClient()
            let endpoint = try await NotificationReceiptClient().healthyEndpoint()
            try Task.checkCancellation()
            let id = try await client.start(at: endpoint)
            operation.call("POST /mail/sync", result: "accepted")
            while !Task.isCancelled {
                let update = try await client.progress(id, at: endpoint)
                try Task.checkCancellation()
                snapshot = update
                if update.phase.isTerminal {
                    CenterLog.shared.finish(operation, result: update.error == nil ? "success" : "failure")
                    break
                }
                try await Task.sleep(for: .milliseconds(500))
            }
        } catch {
            if !Task.isCancelled {
                connectionError = error.localizedDescription
                CenterLog.shared.finish(operation, result: "failure")
            }
        }
        if Task.isCancelled {
            CenterLog.shared.finish(operation, result: "cancelled")
        } else {
            finished()
        }
    }
}

private struct MailSyncSnapshot: Decodable {
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
            case .downloading: "正在下载邮件"
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
    }
}

private struct MailSyncClient {
    private struct StartResponse: Decodable {
        let api_version: String
        let id: UUID
    }

    private struct ProgressResponse: Decodable {
        let api_version: String
        let sync: MailSyncSnapshot
    }

    private struct RequestError: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    func start(at endpoint: ServiceEndpoint) async throws -> UUID {
        let data = try await request("POST", path: "/mail/sync", at: endpoint, status: 202)
        let result = try JSONDecoder().decode(StartResponse.self, from: data)
        guard result.api_version == "1" else { throw URLError(.badServerResponse) }
        return result.id
    }

    func progress(_ id: UUID, at endpoint: ServiceEndpoint) async throws -> MailSyncSnapshot {
        let data = try await request("GET", path: "/mail/sync/\(id.uuidString)", at: endpoint, status: 200)
        let result = try JSONDecoder().decode(ProgressResponse.self, from: data)
        guard result.api_version == "1", result.sync.id == id else { throw URLError(.badServerResponse) }
        return result.sync
    }

    private func request(
        _ method: String, path: String, at endpoint: ServiceEndpoint, status: Int
    ) async throws -> Data {
        let url = try NotificationReceiptClient.url(endpoint: endpoint, path: path)
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 5)
        request.httpMethod = method
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        guard http.statusCode == status else {
            let error = try? JSONDecoder().decode(ServiceAPIErrorResponse.self, from: data)
            throw RequestError(message: error?.error.message ?? "邮件同步请求失败（HTTP \(http.statusCode)）")
        }
        return data
    }
}

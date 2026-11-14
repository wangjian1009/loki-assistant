import Foundation
import Observation

@MainActor
@Observable
final class ApplicationModel {
    private(set) var connectionMessage = "尚未连接 Emacs"
    private(set) var isCheckingConnection = false
    private(set) var sessions: [AgentSession] = []
    private(set) var sessionMessage = "尚未加载会话"
    private(set) var isLoadingSessions = false

    func checkConnection(configuration: EmacsConfiguration) async {
        guard !isCheckingConnection else { return }
        isCheckingConnection = true
        connectionMessage = "正在连接…"
        defer { isCheckingConnection = false }

        do {
            let info = try await EmacsClient().ping(configuration: configuration)
            connectionMessage = "已连接 Emacs \(info.emacsVersion) · 插件 \(info.pluginVersion)"
        } catch {
            connectionMessage = error.localizedDescription
        }
    }

    func refreshSessions(configuration: EmacsConfiguration) async {
        guard !isLoadingSessions else { return }
        isLoadingSessions = true
        sessionMessage = "正在读取会话…"
        defer { isLoadingSessions = false }

        do {
            let result = try await EmacsClient().listSessions(configuration: configuration)
            sessions = result.sessions
            sessionMessage = result.sessions.isEmpty ? "当前没有 agent-shell 会话" : ""
        } catch {
            sessionMessage = error.localizedDescription
        }
    }
}

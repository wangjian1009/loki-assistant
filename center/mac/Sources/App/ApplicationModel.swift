import AppKit
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
    private(set) var selectedSessionID: String?
    private(set) var isSelectingSession = false

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

    func selectSession(sessionID: String, configuration: EmacsConfiguration) async {
        guard !isSelectingSession else { return }
        isSelectingSession = true
        defer { isSelectingSession = false }

        do {
            let result = try await EmacsClient().selectSession(
                configuration: configuration,
                sessionID: sessionID
            )
            selectedSessionID = result.sessionID
            let bufferName = sessions.first { $0.id == result.sessionID }?.bufferName ?? result.sessionID
            sessionMessage = "已显示会话：\(bufferName)"
            activateEmacs()
        } catch {
            sessionMessage = error.localizedDescription
        }
    }

    private func activateEmacs() {
        let application = NSWorkspace.shared.runningApplications.first { application in
            application.bundleIdentifier?.hasPrefix("org.gnu.Emacs") == true
                || application.localizedName == "Emacs"
        }
        _ = application?.activate()
    }
}

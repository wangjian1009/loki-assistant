import AppKit
import Foundation
import Observation

@MainActor
@Observable
final class ApplicationModel {
    enum ConnectionState {
        case unchecked, connected, failed
    }

    private let client: any EmacsCommunicating
    private let activateApplication: @MainActor (Int32) -> Bool
    private(set) var configuration: EmacsConfiguration?
    private var configurationGeneration = UUID()
    private(set) var connectionMessage = "尚未连接 Emacs"
    private(set) var isCheckingConnection = false
    private(set) var connectionState = ConnectionState.unchecked
    private(set) var sessions: [AgentSession] = []
    private(set) var sessionMessage = "尚未加载会话"
    private(set) var isLoadingSessions = false
    private(set) var lastSessionsRefresh: Date?
    private(set) var sessionRefreshError: String?
    private(set) var selectedSessionID: String?
    private(set) var isSelectingSession = false
    var promptText = ""
    private(set) var isSendingPrompt = false

    init(
        client: any EmacsCommunicating = EmacsClient(),
        activateApplication: @escaping @MainActor (Int32) -> Bool = { processID in
            NSRunningApplication(processIdentifier: processID)?.activate() ?? false
        }
    ) {
        self.client = client
        self.activateApplication = activateApplication
    }

    func updateConfiguration(_ configuration: EmacsConfiguration) {
        guard self.configuration != configuration else { return }
        let hadConfiguration = self.configuration != nil
        self.configuration = configuration
        configurationGeneration = UUID()
        connectionMessage = "尚未连接 Emacs"
        connectionState = .unchecked
        sessions = []
        lastSessionsRefresh = nil
        sessionRefreshError = nil
        selectedSessionID = nil
        sessionMessage = hadConfiguration ? "连接设置已更改，请刷新会话" : "尚未加载会话"
        isCheckingConnection = false
        isLoadingSessions = false
        isSelectingSession = false
        isSendingPrompt = false
    }

    func checkConnection(configuration: EmacsConfiguration) async {
        updateConfiguration(configuration)
        guard !isCheckingConnection else { return }
        let generation = configurationGeneration
        isCheckingConnection = true
        connectionMessage = "正在连接…"
        defer {
            if configurationGeneration == generation { isCheckingConnection = false }
        }

        do {
            let info = try await client.ping(configuration: configuration)
            guard configurationGeneration == generation else { return }
            connectionMessage = "已连接 Emacs \(info.emacsVersion) · 插件 \(info.pluginVersion)"
            connectionState = .connected
        } catch {
            guard configurationGeneration == generation else { return }
            connectionMessage = error.localizedDescription
            connectionState = .failed
        }
    }

    func refreshStatus(configuration: EmacsConfiguration) async {
        async let connection: Void = checkConnection(configuration: configuration)
        async let sessions: Void = refreshSessions(configuration: configuration)
        _ = await (connection, sessions)
    }

    func refreshSessions(configuration: EmacsConfiguration) async {
        updateConfiguration(configuration)
        guard !isLoadingSessions else { return }
        let generation = configurationGeneration
        isLoadingSessions = true
        sessionMessage = "正在读取会话…"
        defer {
            if configurationGeneration == generation { isLoadingSessions = false }
        }

        do {
            let result = try await client.listSessions(configuration: configuration)
            guard configurationGeneration == generation else { return }
            sessions = result.sessions
            lastSessionsRefresh = Date()
            sessionRefreshError = nil
            sessionMessage = result.sessions.isEmpty ? "当前没有 agent-shell 会话" : ""
            if let selectedSessionID, !sessions.contains(where: { $0.id == selectedSessionID }) {
                self.selectedSessionID = nil
                if !sessions.isEmpty {
                    sessionMessage = "原选会话已失效，请重新选择"
                }
            }
        } catch {
            guard configurationGeneration == generation else { return }
            sessionMessage = error.localizedDescription
            sessionRefreshError = error.localizedDescription
        }
    }

    func focusSession(sessionID: String) {
        guard !isSelectingSession, !isSendingPrompt,
              sessions.contains(where: { $0.id == sessionID }) else { return }
        selectedSessionID = sessionID
        sessionMessage = sessionRefreshError ?? ""
    }

    func selectSession(sessionID: String, configuration: EmacsConfiguration) async {
        updateConfiguration(configuration)
        guard !isSelectingSession else { return }
        guard sessions.contains(where: { $0.id == sessionID }) else {
            sessionMessage = "请先刷新会话列表并重新选择"
            return
        }
        let generation = configurationGeneration
        isSelectingSession = true
        defer {
            if configurationGeneration == generation { isSelectingSession = false }
        }

        do {
            let result = try await client.selectSession(
                configuration: configuration,
                sessionID: sessionID
            )
            guard configurationGeneration == generation else { return }
            selectedSessionID = result.sessionID
            let bufferName = sessions.first { $0.id == result.sessionID }?.bufferName ?? result.sessionID
            if activateApplication(result.emacsPID) {
                sessionMessage = "已激活会话：\(bufferName)"
            } else {
                sessionMessage = "已显示会话，但无法激活 Emacs：\(bufferName)"
            }
        } catch {
            guard configurationGeneration == generation else { return }
            if case EmacsClientError.remote(code: "session-not-found", message: _) = error {
                removeSession(sessionID)
            }
            sessionMessage = error.localizedDescription
        }
    }

    private func removeSession(_ sessionID: String) {
        sessions.removeAll { $0.id == sessionID }
        if selectedSessionID == sessionID {
            selectedSessionID = nil
        }
    }

    func sendPrompt(configuration: EmacsConfiguration) async {
        updateConfiguration(configuration)
        guard !isSendingPrompt else { return }
        guard let selectedSessionID else {
            sessionMessage = "请先选择 agent-shell 会话"
            return
        }
        guard !promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            sessionMessage = "需求不能为空"
            return
        }

        let prompt = promptText
        let generation = configurationGeneration
        isSendingPrompt = true
        sessionMessage = "正在发送需求…"
        defer {
            if configurationGeneration == generation { isSendingPrompt = false }
        }

        do {
            let result = try await client.sendPrompt(
                configuration: configuration,
                sessionID: selectedSessionID,
                prompt: prompt
            )
            guard configurationGeneration == generation else { return }
            guard result.accepted else {
                sessionMessage = "agent-shell 未接受需求"
                return
            }
            if promptText == prompt {
                promptText = ""
            }
            sessionMessage = "需求已提交"
        } catch {
            guard configurationGeneration == generation else { return }
            if case EmacsClientError.remote(code: "session-not-found", message: _) = error {
                removeSession(selectedSessionID)
            }
            sessionMessage = error.localizedDescription
        }
    }
}

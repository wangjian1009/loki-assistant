import XCTest
@testable import LokiAssistant

final class ApplicationModelConfigurationTests: XCTestCase {
    private let firstServer = EmacsConfiguration(executablePath: "/unused/emacsclient", socketName: "first")
    private let secondServer = EmacsConfiguration(executablePath: "/unused/emacsclient", socketName: "second")
    private let sessions = SessionList(sessions: [
        AgentSession(id: "shared", bufferName: "会话", projectPath: nil, status: "ready")
    ])

    @MainActor
    func testOldListCannotReplaceNewServerOrStopItsLoadingIndicator() async {
        let client = DeferredEmacsClient()
        let model = ApplicationModel(client: client)
        let first = Task { await model.refreshSessions(configuration: firstServer) }
        await client.waitFor(.list, server: "first")
        let second = Task { await model.refreshSessions(configuration: secondServer) }
        await client.waitFor(.list, server: "second")
        await client.complete(.list, server: "first", result: .success(.sessions(sessions)))
        await first.value
        XCTAssertTrue(model.sessions.isEmpty)
        XCTAssertTrue(model.isLoadingSessions)
        XCTAssertEqual(model.sessionMessage, "正在读取会话…")
        await client.complete(.list, server: "second", result: .success(.sessions(SessionList(sessions: []))))
        await second.value
        XCTAssertFalse(model.isLoadingSessions)
        XCTAssertEqual(model.sessionMessage, "当前没有 agent-shell 会话")
    }

    @MainActor
    func testReturningToOriginalConfigurationDoesNotAcceptItsOldResponse() async {
        let client = DeferredEmacsClient()
        let model = ApplicationModel(client: client)
        let request = Task { await model.refreshSessions(configuration: firstServer) }
        await client.waitFor(.list, server: "first")
        model.updateConfiguration(secondServer)
        model.updateConfiguration(firstServer)
        await client.complete(.list, server: "first", result: .success(.sessions(sessions)))
        await request.value
        XCTAssertTrue(model.sessions.isEmpty)
        XCTAssertEqual(model.sessionMessage, "连接设置已更改，请刷新会话")
    }

    @MainActor
    func testOldSelectionCannotActivatePreviousEmacs() async {
        let client = DeferredEmacsClient()
        var activatedPIDs: [Int32] = []
        let model = ApplicationModel(client: client, activateApplication: {
            activatedPIDs.append($0)
            return true
        })
        await loadSessions(model, client: client, configuration: firstServer)
        let first = Task { await model.selectSession(sessionID: "shared", configuration: firstServer) }
        await client.waitFor(.select, server: "first")
        await loadSessions(model, client: client, configuration: secondServer)
        XCTAssertNil(model.selectedSessionID)
        let second = Task { await model.selectSession(sessionID: "shared", configuration: secondServer) }
        await client.waitFor(.select, server: "second")
        await client.complete(.select, server: "first", result: .success(.selected(SelectedSession(
            sessionID: "shared", emacsPID: 1234
        ))))
        await first.value
        XCTAssertTrue(activatedPIDs.isEmpty)
        XCTAssertNil(model.selectedSessionID)
        XCTAssertTrue(model.isSelectingSession)
        await client.complete(.select, server: "second", result: .success(.selected(SelectedSession(
            sessionID: "shared", emacsPID: 5678
        ))))
        await second.value
        XCTAssertEqual(activatedPIDs, [5678])
        XCTAssertEqual(model.selectedSessionID, "shared")
    }

    @MainActor
    func testOldFailureCannotRemoveSameIDFromNewServer() async {
        let client = DeferredEmacsClient()
        let model = ApplicationModel(client: client, activateApplication: { _ in true })
        await loadSessions(model, client: client, configuration: firstServer)
        let first = Task { await model.selectSession(sessionID: "shared", configuration: firstServer) }
        await client.waitFor(.select, server: "first")
        await loadSessions(model, client: client, configuration: secondServer)
        await selectSession(model, client: client, configuration: secondServer)
        let message = model.sessionMessage
        await client.complete(.select, server: "first", result: .failure(EmacsClientError.remote(
            code: "session-not-found", message: "目标会话已失效"
        )))
        await first.value
        XCTAssertEqual(model.sessions.map(\.id), ["shared"])
        XCTAssertEqual(model.selectedSessionID, "shared")
        XCTAssertEqual(model.sessionMessage, message)
    }

    @MainActor
    func testOldSendCannotClearDraftOrStopNewServerSend() async {
        let client = DeferredEmacsClient()
        let model = ApplicationModel(client: client, activateApplication: { _ in true })
        await loadSessions(model, client: client, configuration: firstServer)
        await selectSession(model, client: client, configuration: firstServer)
        model.promptText = "保留草稿"
        let first = Task { await model.sendPrompt(configuration: firstServer) }
        await client.waitFor(.send, server: "first")
        model.updateConfiguration(secondServer)
        XCTAssertTrue(model.sessions.isEmpty)
        XCTAssertNil(model.selectedSessionID)
        XCTAssertEqual(model.promptText, "保留草稿")
        await loadSessions(model, client: client, configuration: secondServer)
        await selectSession(model, client: client, configuration: secondServer)
        let second = Task { await model.sendPrompt(configuration: secondServer) }
        await client.waitFor(.send, server: "second")
        await client.complete(.send, server: "first", result: .success(.accepted(AcceptedPrompt(
            sessionID: "shared", accepted: true
        ))))
        await first.value
        XCTAssertTrue(model.isSendingPrompt)
        XCTAssertEqual(model.promptText, "保留草稿")
        XCTAssertEqual(model.sessionMessage, "正在发送需求…")
        await client.complete(.send, server: "second", result: .failure(EmacsClientError.remote(
            code: "session-not-ready", message: "目标会话当前无法接收需求"
        )))
        await second.value
        XCTAssertFalse(model.isSendingPrompt)
        XCTAssertEqual(model.promptText, "保留草稿")
    }

    @MainActor
    func testOldPingFailureCannotOverwriteNewConnection() async {
        let client = DeferredEmacsClient()
        let model = ApplicationModel(client: client)
        let first = Task { await model.checkConnection(configuration: firstServer) }
        await client.waitFor(.ping, server: "first")
        let second = Task { await model.checkConnection(configuration: secondServer) }
        await client.waitFor(.ping, server: "second")
        await client.complete(.ping, server: "first", result: .failure(EmacsClientError.timedOut))
        await first.value
        XCTAssertTrue(model.isCheckingConnection)
        XCTAssertEqual(model.connectionMessage, "正在连接…")
        await client.complete(.ping, server: "second", result: .success(.connection(ConnectionInfo(
            emacsVersion: "new-server", pluginVersion: "0.2.0"
        ))))
        await second.value
        XCTAssertFalse(model.isCheckingConnection)
        XCTAssertTrue(model.connectionMessage.contains("new-server"))
    }

    @MainActor
    private func loadSessions(
        _ model: ApplicationModel, client: DeferredEmacsClient, configuration: EmacsConfiguration
    ) async {
        let request = Task { await model.refreshSessions(configuration: configuration) }
        await client.waitFor(.list, server: configuration.socketName)
        await client.complete(.list, server: configuration.socketName, result: .success(.sessions(sessions)))
        await request.value
    }

    @MainActor
    private func selectSession(
        _ model: ApplicationModel, client: DeferredEmacsClient, configuration: EmacsConfiguration
    ) async {
        let request = Task { await model.selectSession(sessionID: "shared", configuration: configuration) }
        await client.waitFor(.select, server: configuration.socketName)
        await client.complete(.select, server: configuration.socketName, result: .success(.selected(SelectedSession(
            sessionID: "shared", emacsPID: 5678
        ))))
        await request.value
    }
}

private actor DeferredEmacsClient: EmacsCommunicating {
    enum Operation: Hashable { case ping, list, select, send }
    enum Response: Sendable {
        case connection(ConnectionInfo), sessions(SessionList), selected(SelectedSession), accepted(AcceptedPrompt)
    }
    private struct Key: Hashable {
        let operation: Operation
        let server: String
    }
    private var pending: [Key: CheckedContinuation<Response, any Error>] = [:]
    private var waiting: [Key: CheckedContinuation<Void, Never>] = [:]

    func waitFor(_ operation: Operation, server: String) async {
        let key = Key(operation: operation, server: server)
        guard pending[key] == nil else { return }
        await withCheckedContinuation { waiting[key] = $0 }
    }

    func complete(_ operation: Operation, server: String, result: Result<Response, any Error>) {
        pending.removeValue(forKey: Key(operation: operation, server: server))?.resume(with: result)
    }

    private func request(_ operation: Operation, configuration: EmacsConfiguration) async throws -> Response {
        let key = Key(operation: operation, server: configuration.socketName)
        return try await withCheckedThrowingContinuation {
            pending[key] = $0
            waiting.removeValue(forKey: key)?.resume()
        }
    }

    func ping(configuration: EmacsConfiguration) async throws -> ConnectionInfo {
        guard case let .connection(result) = try await request(.ping, configuration: configuration) else {
            throw EmacsClientError.invalidResponse
        }
        return result
    }

    func listSessions(configuration: EmacsConfiguration) async throws -> SessionList {
        guard case let .sessions(result) = try await request(.list, configuration: configuration) else {
            throw EmacsClientError.invalidResponse
        }
        return result
    }

    func selectSession(configuration: EmacsConfiguration, sessionID: String) async throws -> SelectedSession {
        guard case let .selected(result) = try await request(.select, configuration: configuration) else {
            throw EmacsClientError.invalidResponse
        }
        return result
    }

    func sendPrompt(configuration: EmacsConfiguration, sessionID: String, prompt: String) async throws -> AcceptedPrompt {
        guard case let .accepted(result) = try await request(.send, configuration: configuration) else {
            throw EmacsClientError.invalidResponse
        }
        return result
    }
}

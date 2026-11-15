import XCTest
@testable import LokiAssistant

final class ApplicationModelTests: XCTestCase {
    private let configuration = EmacsConfiguration(executablePath: "/unused/emacsclient", socketName: "")

    @MainActor
    func testRefreshClearsClosedSelectionAndPreservesDraft() async {
        let client = SessionClientStub()
        let model = ApplicationModel(client: client, activateApplication: { _ in true })
        await model.refreshSessions(configuration: configuration)
        await model.selectSession(sessionID: "first", configuration: configuration)
        model.promptText = "保留未发送需求"

        await client.setSessions([SessionClientStub.second])
        await model.refreshSessions(configuration: configuration)
        XCTAssertNil(model.selectedSessionID)
        XCTAssertEqual(model.sessions.map(\.id), ["second"])
        XCTAssertEqual(model.promptText, "保留未发送需求")
        XCTAssertEqual(model.sessionMessage, "原选会话已失效，请重新选择")

        await model.sendPrompt(configuration: configuration)
        let sendCount = await client.sendCount
        XCTAssertEqual(sendCount, 0)
        XCTAssertEqual(model.promptText, "保留未发送需求")
    }

    @MainActor
    func testClickingExpiredSessionPreservesOtherSelection() async {
        let client = SessionClientStub()
        var activatedPIDs: [Int32] = []
        let model = ApplicationModel(client: client, activateApplication: { processID in
            activatedPIDs.append(processID)
            return true
        })
        await model.refreshSessions(configuration: configuration)
        await model.selectSession(sessionID: "first", configuration: configuration)
        model.promptText = "草稿"
        await client.setSessions([SessionClientStub.first])
        await model.selectSession(sessionID: "second", configuration: configuration)

        XCTAssertEqual(model.selectedSessionID, "first")
        XCTAssertEqual(model.sessions.map(\.id), ["first"])
        XCTAssertEqual(activatedPIDs, [1234])
        XCTAssertEqual(model.promptText, "草稿")
        XCTAssertTrue(model.sessionMessage.contains("session-not-found"))

        await client.setSessions([])
        await model.selectSession(sessionID: "first", configuration: configuration)
        XCTAssertNil(model.selectedSessionID)
        XCTAssertTrue(model.sessions.isEmpty)
        XCTAssertEqual(activatedPIDs, [1234])
    }

    @MainActor
    func testRefreshFailurePreservesSelectionAndRecoversToEmptyList() async {
        let client = SessionClientStub()
        let model = ApplicationModel(client: client, activateApplication: { _ in true })
        await model.refreshSessions(configuration: configuration)
        await model.selectSession(sessionID: "first", configuration: configuration)
        model.promptText = "草稿"
        await client.setListError(.timedOut)
        await model.refreshSessions(configuration: configuration)
        XCTAssertEqual(model.selectedSessionID, "first")
        XCTAssertEqual(model.sessions.count, 2)
        XCTAssertEqual(model.sessionMessage, EmacsClientError.timedOut.localizedDescription)
        XCTAssertFalse(model.isLoadingSessions)

        await client.setListError(nil)
        await client.setSessions([])
        await model.refreshSessions(configuration: configuration)
        XCTAssertNil(model.selectedSessionID)
        XCTAssertTrue(model.sessions.isEmpty)
        XCTAssertEqual(model.sessionMessage, "当前没有 agent-shell 会话")
        XCTAssertEqual(model.promptText, "草稿")
    }

    @MainActor
    func testSelectionActivatesReturnedProcessAndReportsFailure() async {
        let client = SessionClientStub()
        var activatedPIDs: [Int32] = []
        let model = ApplicationModel(client: client, activateApplication: { processID in
            activatedPIDs.append(processID)
            return processID == 1234
        })
        await model.refreshSessions(configuration: configuration)
        await model.selectSession(sessionID: "second", configuration: configuration)
        XCTAssertEqual(model.selectedSessionID, "second")
        XCTAssertEqual(activatedPIDs, [5678])
        XCTAssertTrue(model.sessionMessage.contains("无法激活 Emacs"))

        await model.selectSession(sessionID: "first", configuration: configuration)
        XCTAssertEqual(model.selectedSessionID, "first")
        XCTAssertEqual(activatedPIDs, [5678, 1234])
        XCTAssertTrue(model.sessionMessage.hasPrefix("已激活会话："))
    }

    @MainActor
    func testExpiredSendTargetClearsSelectionAndKeepsDraft() async {
        let client = SessionClientStub()
        let model = ApplicationModel(client: client, activateApplication: { _ in true })
        await model.refreshSessions(configuration: configuration)
        await model.selectSession(sessionID: "first", configuration: configuration)
        model.promptText = "保留未发送需求"
        await client.setSessions([])
        await model.sendPrompt(configuration: configuration)
        XCTAssertNil(model.selectedSessionID)
        XCTAssertEqual(model.sessions.map(\.id), ["second"])
        XCTAssertEqual(model.promptText, "保留未发送需求")
        XCTAssertFalse(model.isSendingPrompt)
        XCTAssertTrue(model.sessionMessage.contains("session-not-found"))
    }
}

private actor SessionClientStub: EmacsCommunicating {
    static let first = AgentSession(id: "first", bufferName: "原会话", projectPath: nil, status: "ready")
    static let second = AgentSession(id: "second", bufferName: "其他会话", projectPath: nil, status: "busy")
    private var sessions = [first, second]
    private var listError: EmacsClientError?
    private(set) var sendCount = 0

    func setSessions(_ sessions: [AgentSession]) { self.sessions = sessions }
    func setListError(_ error: EmacsClientError?) { listError = error }

    func ping(configuration: EmacsConfiguration) async throws -> ConnectionInfo {
        ConnectionInfo(emacsVersion: "test", pluginVersion: "test")
    }

    func listSessions(configuration: EmacsConfiguration) async throws -> SessionList {
        if let listError { throw listError }
        return SessionList(sessions: sessions)
    }

    func selectSession(configuration: EmacsConfiguration, sessionID: String) async throws -> SelectedSession {
        guard sessions.contains(where: { $0.id == sessionID }) else {
            throw EmacsClientError.remote(code: "session-not-found", message: "目标会话已失效")
        }
        return SelectedSession(sessionID: sessionID, emacsPID: sessionID == "first" ? 1234 : 5678)
    }

    func sendPrompt(configuration: EmacsConfiguration, sessionID: String, prompt: String) async throws -> AcceptedPrompt {
        sendCount += 1
        throw EmacsClientError.remote(code: "session-not-found", message: "目标会话已失效")
    }
}

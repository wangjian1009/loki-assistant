import Foundation
import XCTest
@testable import LokiAssistant

final class EmacsConnectionIntegrationTests: XCTestCase {
    @MainActor
    func testRealConnectionAndRecovery() async throws {
        let environment = ProcessInfo.processInfo.environment
        try XCTSkipUnless(environment["LOKI_TEST_CONNECTION"] == "1", "通过 make test-connection 执行真实 Emacs 联调")
        let configuration = EmacsConfiguration(
            executablePath: environment["LOKI_EMACSCLIENT"] ?? EmacsConfiguration.defaultExecutablePath,
            socketName: environment["LOKI_EMACS_SOCKET"] ?? ""
        )
        let info = try await EmacsClient().ping(configuration: configuration)
        XCTAssertFalse(info.emacsVersion.isEmpty)
        XCTAssertFalse(info.pluginVersion.isEmpty)

        let sessionList = try await EmacsClient().listSessions(configuration: configuration)
        XCTAssertTrue(sessionList.sessions.allSatisfy { session in
            !session.id.isEmpty && !session.bufferName.isEmpty
                && ["ready", "busy", "unknown"].contains(session.status)
        })

        let model = ApplicationModel()
        await model.checkConnection(configuration: EmacsConfiguration(
            executablePath: "/loki-assistant-missing/emacsclient", socketName: configuration.socketName
        ))
        XCTAssertEqual(model.connectionMessage, EmacsClientError.executableNotFound.localizedDescription)
        XCTAssertFalse(model.isCheckingConnection)

        await model.checkConnection(configuration: EmacsConfiguration(
            executablePath: configuration.executablePath,
            socketName: NSTemporaryDirectory() + "loki-assistant-missing-" + UUID().uuidString
        ))
        XCTAssertTrue(model.connectionMessage.hasPrefix("连接 Emacs 失败："), model.connectionMessage)
        XCTAssertFalse(model.isCheckingConnection)

        await model.checkConnection(configuration: configuration)
        XCTAssertEqual(model.connectionMessage, "已连接 Emacs \(info.emacsVersion) · 插件 \(info.pluginVersion)")
        XCTAssertFalse(model.isCheckingConnection)
        print("真实连接结果：\(model.connectionMessage)")
    }
}

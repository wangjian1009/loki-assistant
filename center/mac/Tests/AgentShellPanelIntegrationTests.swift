import AppKit
import SwiftUI
import XCTest
@testable import LokiAssistant

final class AgentShellPanelIntegrationTests: XCTestCase {
    @MainActor
    func testSessionPanelLoadsSelectsAndRefreshesOnReopen() async throws {
        let environment = ProcessInfo.processInfo.environment
        try XCTSkipUnless(environment["LOKI_TEST_CONNECTION"] == "1", "通过 make test-connection 验证真实会话界面")
        let configuration = EmacsConfiguration(
            executablePath: environment["LOKI_EMACSCLIENT"] ?? EmacsConfiguration.defaultExecutablePath,
            socketName: environment["LOKI_EMACS_SOCKET"] ?? ""
        )
        let suiteName = "loki-panel-test-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defaults.set(configuration.executablePath, forKey: EmacsConfiguration.executablePathKey)
        defaults.set(configuration.socketName, forKey: EmacsConfiguration.socketNameKey)
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let model = ApplicationModel()
        model.promptText = "界面验收草稿"
        let host = NSHostingView(rootView: AgentShellView(model: model).defaultAppStorage(defaults))
        let window = NSWindow(
            contentRect: NSRect(x: 200, y: 200, width: 360, height: 650),
            styleMask: [.titled, .closable], backing: .buffered, defer: false
        )
        window.isReleasedWhenClosed = false
        window.contentView = host
        defer { window.close() }
        window.makeKeyAndOrderFront(nil)

        try await waitUntil {
            !model.isLoadingSessions && model.sessionMessage != "尚未加载会话"
        }
        try XCTSkipIf(model.sessions.isEmpty && model.sessionMessage == "当前没有 agent-shell 会话",
                      "当前没有可点击的真实 agent-shell 会话")
        XCTAssertTrue(model.sessionMessage.isEmpty, model.sessionMessage)
        let session = try XCTUnwrap(model.sessions.first)
        try await waitUntil { tables(in: host).first?.numberOfRows == model.sessions.count }
        let table = try XCTUnwrap(tables(in: host).first)
        XCTAssertEqual(table.numberOfRows, model.sessions.count)
        XCTAssertGreaterThanOrEqual(try XCTUnwrap(table.enclosingScrollView).contentView.bounds.height, 200)

        // 保存并恢复用户的 Emacs 窗口布局，联调只切换 buffer，不发送需求。
        let snapshot = "loki-assistant-test-window-" + UUID().uuidString
        let pidOutput = try evaluateEmacs(
            "(progn (set (quote \(snapshot)) (current-window-configuration)) (emacs-pid))",
            configuration: configuration
        )
        defer {
            do {
                _ = try evaluateEmacs(
                    "(unwind-protect (set-window-configuration (symbol-value (quote \(snapshot)))) (makunbound (quote \(snapshot))))",
                    configuration: configuration
                )
            } catch {
                XCTFail("恢复 Emacs 窗口布局失败：\(error.localizedDescription)")
            }
        }
        let emacsPID = try XCTUnwrap(pid_t(pidOutput))

        let row = try XCTUnwrap(table.rowView(atRow: 0, makeIfNecessary: true))
        // 点击行右侧的空白区域，验证整行的点击范围。
        let location = row.convert(NSPoint(x: row.bounds.maxX - 32, y: 10), to: nil)
        for (index, type) in [NSEvent.EventType.leftMouseDown, .leftMouseUp].enumerated() {
            let event = try XCTUnwrap(NSEvent.mouseEvent(
                with: type, location: location, modifierFlags: [],
                timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: window.windowNumber, context: nil,
                eventNumber: index, clickCount: 1, pressure: type == .leftMouseDown ? 1 : 0
            ))
            NSApplication.shared.sendEvent(event)
        }
        try await waitUntil { !model.isSelectingSession && model.selectedSessionID == session.id }
        try await waitUntil { NSWorkspace.shared.frontmostApplication?.processIdentifier == emacsPID }
        XCTAssertTrue(model.sessionMessage.hasPrefix("已激活会话："), model.sessionMessage)
        let encodedID = Data(session.id.utf8).base64EncodedString()
        XCTAssertEqual(try evaluateEmacs(
            "(equal (base64-decode-string \"\(encodedID)\") (agent-shell-session-id :shell-buffer (window-buffer (selected-window))))",
            configuration: configuration
        ), "t")

        window.orderOut(nil)
        await model.refreshSessions(configuration: EmacsConfiguration(
            executablePath: "/loki-panel-test-missing/emacsclient", socketName: ""
        ))
        XCTAssertEqual(model.sessionMessage, EmacsClientError.executableNotFound.localizedDescription)
        window.makeKeyAndOrderFront(nil)
        try await waitUntil { !model.isLoadingSessions && model.sessionMessage.isEmpty }
        XCTAssertEqual(model.selectedSessionID, session.id)
        XCTAssertEqual(model.promptText, "界面验收草稿")
        print("真实界面联调：列出 \(model.sessions.count) 个会话，整行点击已激活 Emacs，重新打开面板已自动刷新并保留草稿")
    }

    @MainActor
    private func tables(in view: NSView) -> [NSTableView] {
        if let table = view as? NSTableView { return [table] }
        return view.subviews.flatMap { tables(in: $0) }
    }

    @MainActor
    private func waitUntil(_ condition: () -> Bool) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while !condition() {
            guard ContinuousClock.now < deadline else {
                XCTFail("等待界面或 Emacs 状态超时")
                throw EmacsClientError.timedOut
            }
            try await Task.sleep(for: .milliseconds(50))
        }
    }

    private func evaluateEmacs(_ expression: String, configuration: EmacsConfiguration) throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: configuration.executablePath)
        var arguments = ["--alternate-editor=false", "--timeout=5"]
        if !configuration.socketName.isEmpty {
            arguments += ["--socket-name", configuration.socketName]
        }
        process.arguments = arguments + ["--eval", "\n  \(expression)\n"]
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output
        try process.run()
        let data = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let text = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        guard process.terminationStatus == 0 else {
            throw EmacsClientError.processFailed(text)
        }
        return text
    }
}

import AppKit
import SwiftUI
import XCTest
@testable import LokiAssistant

final class AgentShellWindowTests: XCTestCase {
    @MainActor
    func testDashboardOpensSingleWorkspaceAndPreservesDraftOnReopen() async throws {
        let previousApplication = NSWorkspace.shared.frontmostApplication
        let frameKey = "NSWindow Frame AgentShellWorkspace"
        let previousFrame = UserDefaults.standard.object(forKey: frameKey)
        UserDefaults.standard.removeObject(forKey: frameKey)
        let model = ApplicationModel(client: WorkspaceClientStub())
        let host = NSHostingView(rootView: DashboardView(model: model))
        let panel = NSPanel(
            contentRect: NSRect(x: 200, y: 200, width: 340, height: 400),
            styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false
        )
        panel.isReleasedWhenClosed = false
        panel.level = .popUpMenu
        panel.hidesOnDeactivate = false
        panel.contentView = host
        let presenter = AgentShellWindowPresenter.shared
        defer {
            presenter.window?.close()
            panel.close()
            if let previousFrame {
                UserDefaults.standard.set(previousFrame, forKey: frameKey)
            } else {
                UserDefaults.standard.removeObject(forKey: frameKey)
            }
            previousApplication?.activate()
        }
        XCTAssertNil(presenter.window, "启动时不应创建会话窗口")
        panel.orderFrontRegardless()
        try await waitUntil { model.lastSessionsRefresh != nil && !model.isCheckingConnection }
        XCTAssertEqual(model.connectionState, .connected)
        XCTAssertEqual(model.sessions.count, 3)
        try saveSnapshot(host, name: "loki-dashboard")
        XCTAssertNil(find(NSTableView.self, in: host), "仪表盘不承载会话列表")
        XCTAssertNil(find(NSTextView.self, in: host), "仪表盘不承载需求编辑器")
        let button = try XCTUnwrap(findButton("open-agent-shell", in: host))
        try click(button, in: panel)
        try await waitUntil { presenter.window?.isVisible == true }
        try await waitUntil { presenter.window?.isKeyWindow == true }
        let window = try XCTUnwrap(presenter.window)
        XCTAssertFalse(panel.isVisible)
        XCTAssertEqual(window.level, .normal)
        XCTAssertTrue(window.styleMask.contains(.resizable))
        let content = try XCTUnwrap(window.contentView)
        try await waitUntil { self.find(NSTableView.self, in: content)?.numberOfRows == 3 }
        XCTAssertNotNil(find(NSTextView.self, in: content))

        model.focusSession(sessionID: "first")
        model.promptText = "请梳理窗口生命周期，并保留尚未发送的草稿。"
        window.layoutIfNeeded()
        try await Task.sleep(for: .milliseconds(150))
        // 保留实际 SwiftUI 视图快照，便于检查双栏布局和最小尺寸。
        try saveSnapshot(content, name: "loki-agent-shell-workspace")
        window.setContentSize(NSSize(width: 780, height: 560))
        window.layoutIfNeeded()
        try await Task.sleep(for: .milliseconds(150))
        try saveSnapshot(content, name: "loki-agent-shell-workspace-compact")
        XCTAssertGreaterThan(try XCTUnwrap(find(NSTableView.self, in: content)?.enclosingScrollView)
            .contentView.bounds.height, 200)

        let settingsButton = try XCTUnwrap(findButton("open-settings", in: content))
        try click(settingsButton, in: window)
        try await waitUntil { SettingsWindowPresenter.shared.window?.isVisible == true }
        XCTAssertTrue(window.isVisible, "从会话窗口打开设置应保留工作窗口")
        SettingsWindowPresenter.shared.window?.close()

        window.orderBack(nil)
        panel.orderFrontRegardless()
        try click(button, in: panel)
        try await waitUntil { window.isKeyWindow && !panel.isVisible }
        XCTAssertTrue(presenter.window === window)

        window.close()
        let refreshDate = model.lastSessionsRefresh
        panel.orderFrontRegardless()
        try click(button, in: panel)
        try await waitUntil { window.isKeyWindow && model.lastSessionsRefresh != refreshDate }
        XCTAssertTrue(presenter.window === window)
        XCTAssertEqual(model.selectedSessionID, "first")
        XCTAssertEqual(model.promptText, "请梳理窗口生命周期，并保留尚未发送的草稿。")
        XCTAssertEqual(NSApplication.shared.activationPolicy(), .accessory)
    }

    @MainActor
    private func click(_ button: NSButton, in window: NSWindow) throws {
        let point = button.convert(NSPoint(x: button.bounds.midX, y: button.bounds.midY), to: nil)
        for (index, type) in [NSEvent.EventType.leftMouseDown, .leftMouseUp].enumerated() {
            let event = try XCTUnwrap(NSEvent.mouseEvent(
                with: type, location: point, modifierFlags: [],
                timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: window.windowNumber, context: nil,
                eventNumber: index, clickCount: 1, pressure: type == .leftMouseDown ? 1 : 0
            ))
            NSApplication.shared.sendEvent(event)
        }
    }

    @MainActor
    private func find<T: NSView>(_ type: T.Type, in view: NSView) -> T? {
        if let match = view as? T { return match }
        return view.subviews.lazy.compactMap { self.find(type, in: $0) }.first
    }

    @MainActor
    private func findButton(_ identifier: String, in view: NSView) -> NSButton? {
        if let button = view as? NSButton, button.identifier?.rawValue == identifier { return button }
        return view.subviews.lazy.compactMap { self.findButton(identifier, in: $0) }.first
    }

    @MainActor
    private func saveSnapshot(_ view: NSView, name: String) throws {
        let bitmap = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: bitmap)
        let data = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
        let attachment = XCTAttachment(data: data, uniformTypeIdentifier: "public.png")
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        try data.write(to: URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent(name + ".png"))
    }

    @MainActor
    private func waitUntil(
        file: StaticString = #filePath, line: UInt = #line, _ condition: () -> Bool
    ) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while !condition() {
            guard ContinuousClock.now < deadline else {
                XCTFail("等待仪表盘或独立会话窗口超时", file: file, line: line)
                throw EmacsClientError.timedOut
            }
            try await Task.sleep(for: .milliseconds(50))
        }
    }
}

private actor WorkspaceClientStub: EmacsCommunicating {
    func ping(configuration: EmacsConfiguration) async throws -> ConnectionInfo {
        ConnectionInfo(emacsVersion: "31.1", pluginVersion: "0.2.0")
    }

    func listSessions(configuration: EmacsConfiguration) async throws -> SessionList {
        SessionList(sessions: [
            AgentSession(id: "first", bufferName: "Loki Assistant · 窗口与交互设计",
                         projectPath: "/Users/developer/workspace/loki-assistant/", status: "ready"),
            AgentSession(id: "second", bufferName: "项目文档整理",
                         projectPath: "/Users/developer/workspace/notes/", status: "busy"),
            AgentSession(id: "third", bufferName: "代码审查", projectPath: nil, status: "unknown")
        ])
    }

    func selectSession(configuration: EmacsConfiguration, sessionID: String) async throws -> SelectedSession {
        SelectedSession(sessionID: sessionID, emacsPID: 1234)
    }

    func sendPrompt(configuration: EmacsConfiguration, sessionID: String, prompt: String) async throws -> AcceptedPrompt {
        AcceptedPrompt(sessionID: sessionID, accepted: true)
    }
}

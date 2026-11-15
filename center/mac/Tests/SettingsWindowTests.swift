import AppKit
import SwiftUI
import XCTest
@testable import LokiAssistant

final class SettingsWindowTests: XCTestCase {
    @MainActor
    func testOpeningSettingsHidesPopupPanel() async throws {
        let presenter = SettingsWindowPresenter.shared
        let host = NSHostingView(rootView: SettingsButton())
        let panel = NSPanel(
            contentRect: NSRect(x: 200, y: 200, width: 160, height: 80),
            styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false
        )
        panel.isReleasedWhenClosed = false
        panel.level = .popUpMenu
        panel.hidesOnDeactivate = false
        panel.contentView = host
        panel.orderFrontRegardless()
        defer {
            presenter.window?.close()
            panel.close()
        }

        try await waitUntil { settingsButton(in: host) != nil }
        try clickSettings(host: host, window: panel)
        try await waitUntil { presenter.window.map(isInFront) == true }
        XCTAssertFalse(panel.isVisible, "打开设置后应收起菜单栏面板，避免高层级面板遮挡设置窗口")
        XCTAssertEqual(presenter.window?.level, .normal)
    }

    @MainActor
    func testSettingsOpensInFrontAndCanBeReopened() async throws {
        let previousApplication = NSWorkspace.shared.frontmostApplication
        let presenter = SettingsWindowPresenter.shared
        let host = NSHostingView(rootView: SettingsButton())
        let sourceWindow = NSWindow(
            contentRect: NSRect(x: 200, y: 200, width: 160, height: 80),
            styleMask: [.titled, .closable], backing: .buffered, defer: false
        )
        sourceWindow.isReleasedWhenClosed = false
        sourceWindow.contentView = host
        sourceWindow.makeKeyAndOrderFront(nil)
        defer {
            presenter.window?.close()
            sourceWindow.close()
            previousApplication?.activate()
        }

        try await waitUntil { settingsButton(in: host) != nil }
        try clickSettings(host: host, window: sourceWindow)
        try await waitUntil { presenter.window.map(isInFront) == true }
        let settingsWindow = try XCTUnwrap(presenter.window)
        XCTAssertEqual(settingsWindow.level, .normal)
        XCTAssertEqual(NSApplication.shared.activationPolicy(), .accessory)

        // 设置窗口仍存在但已退到后台，再次点击应抬起同一个窗口。
        settingsWindow.orderBack(nil)
        sourceWindow.orderFrontRegardless()
        try await waitUntil { !isInFront(settingsWindow) }
        try clickSettings(host: host, window: sourceWindow)
        try await waitUntil { isInFront(settingsWindow) }
        XCTAssertTrue(presenter.window === settingsWindow)

        settingsWindow.close()
        try await waitUntil { !settingsWindow.isVisible }
        try clickSettings(host: host, window: sourceWindow)
        try await waitUntil { presenter.window.map(isInFront) == true }
        XCTAssertEqual(presenter.window?.level, .normal)
        print("设置窗口验证：首次打开置前、后台窗口再次置前、关闭后重开通过；保留菜单栏应用模式")
    }

    @MainActor
    private func clickSettings(host: NSView, window: NSWindow) throws {
        window.orderFrontRegardless()
        let button = try XCTUnwrap(settingsButton(in: host))
        let location = button.convert(NSPoint(x: button.bounds.midX, y: button.bounds.midY), to: nil)
        for (index, type) in [NSEvent.EventType.leftMouseDown, .leftMouseUp].enumerated() {
            let event = try XCTUnwrap(NSEvent.mouseEvent(
                with: type, location: location, modifierFlags: [],
                timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: window.windowNumber, context: nil,
                eventNumber: index, clickCount: 1, pressure: type == .leftMouseDown ? 1 : 0
            ))
            NSApplication.shared.sendEvent(event)
        }
    }

    @MainActor
    private func settingsButton(in view: NSView) -> NSButton? {
        if let button = view as? NSButton, button.identifier?.rawValue == "open-settings" {
            return button
        }
        for child in view.subviews {
            if let button = settingsButton(in: child) { return button }
        }
        return nil
    }

    @MainActor
    private func isInFront(_ window: NSWindow) -> Bool {
        guard window.isVisible,
              let windows = CGWindowListCopyWindowInfo(
                [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID
              ) as? [[String: Any]],
              let front = windows.first(where: {
                  ($0[kCGWindowLayer as String] as? Int) == 0
                      && ($0[kCGWindowAlpha as String] as? Double ?? 0) > 0
              }) else { return false }
        return front[kCGWindowNumber as String] as? Int == window.windowNumber
    }

    @MainActor
    private func waitUntil(_ condition: () -> Bool) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while !condition() {
            guard ContinuousClock.now < deadline else {
                XCTFail("等待设置窗口置前超时")
                throw EmacsClientError.timedOut
            }
            try await Task.sleep(for: .milliseconds(50))
        }
    }
}

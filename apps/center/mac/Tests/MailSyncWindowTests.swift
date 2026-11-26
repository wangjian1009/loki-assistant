import AppKit
import XCTest
@testable import LokiAssistant

final class MailSyncWindowTests: XCTestCase {
    @MainActor
    func testOnlySyncActionOpensProgressWindow() throws {
        let app = NSApplication.shared
        let delegate = CenterAppDelegate()
        let mailSync = MailSyncWindow()

        XCTAssertFalse(delegate.applicationShouldHandleReopen(app, hasVisibleWindows: false))
        XCTAssertFalse(app.windows.contains { $0.title == "邮件同步" && $0.isVisible })

        mailSync.syncFromMenu()
        let window = try XCTUnwrap(app.windows.first { $0.title == "邮件同步" && $0.isVisible })
        defer { window.close() }

        mailSync.syncFromMenu()
        XCTAssertEqual(app.windows.filter { $0.title == "邮件同步" && $0.isVisible }, [window])
    }
}

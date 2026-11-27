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

    func testSyncEventsDeliverIndexMessagesAndFinalResult() throws {
        let id = UUID()
        var decoder = MailSyncEventDecoder()
        XCTAssertNil(try decoder.decode("event: sync"))
        XCTAssertNil(try decoder.decode(": keep-alive"))
        let progress = try XCTUnwrap(decoder.decode(
            """
            data: {"api_version":"1","sync":{"id":"\(id)","phase":"indexing","messages":["检查 120 封"],"error":null}}
            """
        ))
        XCTAssertEqual(progress.id, id)
        XCTAssertEqual(progress.phase, .indexing)
        XCTAssertFalse(progress.phase.isTerminal)
        XCTAssertEqual(progress.messages, ["检查 120 封"])

        let failure = try XCTUnwrap(decoder.decode(
            """
            data: {"api_version":"1","sync":{"id":"\(id)","phase":"failed","messages":["检查 120 封"],"error":"无法打开索引"}}
            """
        ))
        XCTAssertTrue(failure.phase.isTerminal)
        XCTAssertEqual(failure.error, "无法打开索引")
        XCTAssertThrowsError(try decoder.decode(
            """
            data: {"api_version":"1","sync":{"id":"\(UUID())","phase":"completed","messages":[],"error":null}}
            """
        ))
    }
}

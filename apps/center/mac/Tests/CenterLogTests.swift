import Foundation
import XCTest
@testable import LokiAssistant

final class CenterLogTests: XCTestCase {
    func testRendersNestedCallsAndKeepsOnlyRecentThreeDays() {
        let directory = FileManager.default.temporaryDirectory
            .appending(path: "loki-assistant-log-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let log = CenterLog(fileURL: directory.appending(path: "center.log.org"))
        let now = Date()
        let old = now.addingTimeInterval(-4 * 24 * 60 * 60)

        let oldOperation = log.start("旧操作", date: old)
        oldOperation.call("旧调用", result: "success", date: old)
        log.finish(oldOperation, result: "success", date: old)

        let operation = log.start("打开托盘菜单", date: now)
        operation.call("GET /emacs/status", result: "success", details: ["http_status": "200"], date: now)
        log.finish(operation, result: "success", date: now)

        let content = try! String(contentsOf: directory.appending(path: "center.log.org"), encoding: .utf8)
        XCTAssertTrue(content.contains("打开托盘菜单"))
        XCTAssertTrue(content.contains("GET /emacs/status"))
        XCTAssertTrue(content.contains(":RESULT: success"))
        XCTAssertFalse(content.contains("旧操作"))
    }
}

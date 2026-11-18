import Foundation
import XCTest
@testable import LokiAssistant

final class CenterLogTests: XCTestCase {
    func testRendersNestedCallsAndKeepsLatestFiveHundredEntries() {
        let directory = FileManager.default.temporaryDirectory
            .appending(path: "loki-assistant-log-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let log = CenterLog(fileURL: directory.appending(path: "center.log.org"))
        for index in 0...500 {
            let operation = log.start("操作 \(index)")
            if index == 500 {
                operation.call("GET /status", result: "success", details: ["http_status": "200"])
            }
            log.finish(operation, result: "success")
        }

        let content = try! String(contentsOf: directory.appending(path: "center.log.org"), encoding: .utf8)
        XCTAssertTrue(content.contains("操作 500"))
        XCTAssertTrue(content.contains("GET /status"))
        XCTAssertTrue(content.contains(":RESULT: success"))
        XCTAssertFalse(content.contains("操作 0"))
    }
}

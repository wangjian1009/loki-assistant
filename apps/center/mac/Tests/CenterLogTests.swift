import Foundation
import XCTest
@testable import LokiAssistant

final class CenterLogTests: XCTestCase {
    func testRendersNestedCallsAndKeepsLatestFiveHundredEntries() {
        let directory = FileManager.default.temporaryDirectory
            .appending(path: "loki-assistant-log-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let log = CenterLog(fileURL: directory.appending(path: "center.log"))
        for index in 0...500 {
            let operation = log.start("操作 \(index)")
            if index == 500 {
                operation.call("GET /status", result: "success", details: ["http_status": "200"])
            }
            log.finish(operation, result: "success")
        }

        log.flush()
        let content = try! String(contentsOf: directory.appending(path: "center.log"), encoding: .utf8)
        XCTAssertTrue(content.contains("操作 500"))
        XCTAssertTrue(content.contains("GET /status"))
        XCTAssertTrue(content.contains("] INFO center - 操作 500\n  "))
        XCTAssertTrue(content.contains("result=success"))
        XCTAssertEqual(content.split(separator: "\n").filter { !$0.hasPrefix(" ") }.count, 500)
        XCTAssertFalse(content.contains("操作 0"))
    }
}

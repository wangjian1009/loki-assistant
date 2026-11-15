import UserNotifications
import XCTest
@testable import LokiAssistant

final class SystemNotificationTests: XCTestCase {
    private func url(payload: String) throws -> URL {
        var components = URLComponents()
        components.scheme = "loki-assistant"
        components.host = "emacs"
        components.path = "/notification"
        components.queryItems = [URLQueryItem(name: "payload", value: payload)]
        return try XCTUnwrap(components.url)
    }

    func testDecodesEscapedNotificationAndKeepsBufferAsMetadata() throws {
        let payload = #"{"title":"任务完成 & \"提示\"","icon":"✓","buffer":"*任务 #1*"}"#
        let notification = try EmacsSystemNotification(url: url(payload: payload))
        let content = notification.makeContent()
        XCTAssertEqual(content.title, "✓ 任务完成 & \"提示\"")
        XCTAssertEqual(content.body, "点击激活 Emacs")
        XCTAssertEqual(content.userInfo["buffer"] as? String, "*任务 #1*")
        XCTAssertEqual(content.userInfo["kind"] as? String, "emacs")
    }

    func testRejectsUnrelatedURLsAndInvalidPayloads() throws {
        XCTAssertThrowsError(try EmacsSystemNotification(url: XCTUnwrap(URL(string: "https://example.com"))))
        XCTAssertThrowsError(try EmacsSystemNotification(url: url(payload: #"{"title":"","buffer":"b"}"#)))
        XCTAssertThrowsError(try EmacsSystemNotification(url: url(payload: #"{"title":"任务完成"}"#)))
    }

    func testImagePathDoesNotBecomeTitleText() throws {
        let notification = try EmacsSystemNotification(url: url(payload: #"{"title":"任务完成","icon":"/tmp/icon.png","buffer":"b"}"#))
        XCTAssertEqual(notification.makeContent().title, "任务完成")
    }
}

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
        let notification = try SystemNotification(url: url(payload: payload))
        let content = notification.makeContent()
        XCTAssertEqual(content.title, "✓ 任务完成 & \"提示\"")
        XCTAssertEqual(content.body, "点击激活 Emacs")
        XCTAssertEqual(content.userInfo["buffer"] as? String, "*任务 #1*")
        XCTAssertEqual(content.userInfo["kind"] as? String, "emacs")
    }

    func testRejectsUnrelatedURLsAndInvalidPayloads() throws {
        XCTAssertThrowsError(try SystemNotification(url: XCTUnwrap(URL(string: "https://example.com"))))
        XCTAssertThrowsError(try SystemNotification(url: url(payload: #"{"title":"","buffer":"b"}"#)))
        XCTAssertThrowsError(try SystemNotification(url: url(payload: #"{"title":"任务完成","navigation_url":"https://example.com"}"#)))
    }

    func testImagePathDoesNotBecomeTitleText() throws {
        let notification = try SystemNotification(url: url(payload: #"{"title":"任务完成","icon":"/tmp/icon.png","buffer":"b"}"#))
        XCTAssertEqual(notification.makeContent().title, "任务完成")
    }

    func testGenericNotificationSupportsApplicationIconAndServiceNavigation() throws {
        var components = URLComponents()
        components.scheme = "loki-assistant"
        components.host = "notification"
        components.path = "/show"
        components.queryItems = [URLQueryItem(name: "payload", value: #"{"title":"任务完成","body":"点击显示 Emacs","icon":"app-icon://org.gnu.Emacs","navigation_url":"loki-assistant://notification/activate?callback=%2Femacs%2Factivate"}"#)]
        let notification = try SystemNotification(url: XCTUnwrap(components.url))
        let content = notification.makeContent()
        XCTAssertEqual(content.title, "任务完成")
        XCTAssertEqual(content.body, "点击显示 Emacs")
        XCTAssertEqual(content.userInfo["navigation_url"] as? String, notification.navigationURL)
        XCTAssertNil(content.userInfo["buffer"])
        XCTAssertTrue(SystemNotification.isImage(try XCTUnwrap(notification.icon)))
    }
}

import Darwin
import Foundation
import XCTest
@testable import LokiAssistant

private let receiptTestID = "550e8400-e29b-41d4-a716-446655440000"

final class NotificationReceiptTests: XCTestCase {
    func testStartupRequestsPendingRetryAfterHealthCheck() async throws {
        let stub = ReceiptStub(statuses: [202])
        let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) })
        try await client.requestPendingRetry()
        let requests = stub.requests
        XCTAssertEqual(requests.map { $0.url?.path }, ["/healthz", "/notification/retry-pending"])
        XCTAssertEqual(requests.map { $0.url?.port }, [47321, 47321])
        XCTAssertEqual(requests.last?.httpMethod, "POST")
        XCTAssertNil(requests.last?.httpBody)
    }

    func testStartupSkipsPendingRetryWhenServiceIsUnavailable() async throws {
        let stub = ReceiptStub(statuses: [], healthBody: "{}")
        let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) })
        do {
            try await client.requestPendingRetry()
            XCTFail("健康检查失败时应跳过补发请求")
        } catch NotificationReceiptError.serviceUnavailable {}
        XCTAssertEqual(stub.requests.map { $0.url?.path }, ["/healthz"])
    }

    func testRetriesReceiptWithSameIDAndRediscoversEndpoint() async throws {
        let stub = ReceiptStub(statuses: [500, 200])
        let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) }, retryDelay: { _ in })
        try await client.send(.submitted, notificationID: receiptTestID)
        let requests = stub.requests
        XCTAssertEqual(requests.map { $0.url!.path }, ["/healthz", "/notification/\(receiptTestID)/receipt", "/healthz", "/notification/\(receiptTestID)/receipt"])
        XCTAssertEqual(requests.map { $0.url!.port! }, [47321, 47321, 47322, 47322])
        XCTAssertEqual(requests[1].httpMethod, "POST")
        XCTAssertEqual(requests[1].value(forHTTPHeaderField: "Content-Type"), "application/json")
        XCTAssertEqual(requests[1].httpBody, requests[3].httpBody)
        XCTAssertEqual(try JSONDecoder().decode(NotificationReceipt.self, from: XCTUnwrap(requests[1].httpBody)), .submitted)
    }

    func testStopsRetryingUnknownExpiredAndConflictingReceipts() async throws {
        for status in [400, 404, 409] {
            let stub = ReceiptStub(statuses: [status, 200])
            let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) }, retryDelay: { _ in XCTFail("不应重试确定被拒绝的回执") })
            do {
                try await client.send(.submitted, notificationID: receiptTestID)
                XCTFail("应返回回执错误")
            } catch NotificationReceiptError.rejected(let result) {
                XCTAssertEqual(result, status)
            }
            XCTAssertEqual(stub.requests.count, 2)
        }
    }

    func testBoundsRetriesAfterLostResponses() async throws {
        let stub = ReceiptStub(statuses: [-1, -1, -1])
        let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) }, retryDelay: { _ in })
        do {
            try await client.send(.submitted, notificationID: receiptTestID)
            XCTFail("应返回网络错误")
        } catch is URLError {}
        XCTAssertEqual(stub.requests.filter { $0.httpMethod == "POST" }.count, 3)
    }

    func testDoesNotPostReceiptWhenHealthDoesNotIdentifyServices() async throws {
        let stub = ReceiptStub(statuses: [], healthBody: #"{"service":"another-service","status":"ok","version":"test","uptime_seconds":0}"#)
        let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) }, retryDelay: { _ in })
        do {
            try await client.send(.submitted, notificationID: receiptTestID)
            XCTFail("健康检查失败时不应发送回执")
        } catch NotificationReceiptError.serviceUnavailable {}
        XCTAssertEqual(stub.requests.count, 3)
        XCTAssertTrue(stub.requests.allSatisfy { $0.url?.path == "/healthz" })
    }

    @MainActor
    func testReportsSubmissionAndFailureWithoutPresentingAgainWhenReceiptFails() async throws {
        let url = try notificationURL(#"{"notification_id":"550e8400-e29b-41d4-a716-446655440000","title":"完成","buffer":"中文\"buffer"}"#)
        var presented = 0
        var reported: [NotificationReceipt] = []
        let delivery = NotificationDelivery(present: { notification in
            presented += 1
            XCTAssertEqual(notification.buffer, "中文\"buffer")
            XCTAssertEqual(notification.makeContent().userInfo["notification_id"] as? String, receiptTestID)
        }, report: { id, receipt in
            XCTAssertEqual(id, receiptTestID)
            reported.append(receipt)
            throw URLError(.networkConnectionLost)
        })
        do {
            try await delivery.handle(url)
            XCTFail("应返回回执网络错误")
        } catch is URLError {}
        XCTAssertEqual(presented, 1)
        XCTAssertEqual(reported, [.submitted])
        do {
            try await delivery.handle(url)
            XCTFail("应返回重投后的回执网络错误")
        } catch is URLError {}
        XCTAssertEqual(presented, 1)
        XCTAssertEqual(reported, [.submitted, .submitted])

        let denied = NotificationDelivery(present: { _ in throw SystemNotificationError.permissionDenied }, report: { _, receipt in reported.append(receipt) })
        try await denied.handle(url)
        XCTAssertEqual(reported.last?.status, "failed")
        XCTAssertEqual(reported.last?.code, "permission-denied")
    }

    @MainActor
    func testReportsInvalidPayloadWithValidIDAndAcceptsLegacyURLWithoutReceipt() async throws {
        var receipts: [NotificationReceipt] = []
        var presented = 0
        let delivery = NotificationDelivery(present: { _ in presented += 1 }, report: { _, receipt in receipts.append(receipt) })
        try await delivery.handle(notificationURL(#"{"notification_id":"550e8400-e29b-41d4-a716-446655440000","title":""}"#))
        XCTAssertEqual(presented, 0)
        XCTAssertEqual(receipts.first?.code, "invalid-payload")
        try await delivery.handle(notificationURL(#"{"title":"旧版通知","buffer":"b"}"#))
        XCTAssertEqual(presented, 1)
        XCTAssertEqual(receipts.count, 1)
    }

    func testDiscoveryRequiresHeldServiceLockAndSupportsIPv6() throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "loki-receipt-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let endpoint = ServiceEndpoint(host: "::1", port: 47321, pid: 1, version: "test", startedAt: 0)
        try JSONEncoder().encode(endpoint).write(to: directory.appending(path: "service"))
        XCTAssertThrowsError(try NotificationReceiptClient.discoverEndpoint(directory: directory))
        let descriptor = open(directory.appending(path: "service.lock").path, O_CREAT | O_RDWR, 0o600)
        XCTAssertGreaterThanOrEqual(descriptor, 0)
        defer { close(descriptor) }
        XCTAssertThrowsError(try NotificationReceiptClient.discoverEndpoint(directory: directory))
        XCTAssertEqual(flock(descriptor, LOCK_EX | LOCK_NB), 0)
        XCTAssertEqual(try NotificationReceiptClient.discoverEndpoint(directory: directory), endpoint)
        XCTAssertEqual(try NotificationReceiptClient.url(endpoint: endpoint, path: "/healthz").absoluteString, "http://[::1]:47321/healthz")
        let remote = ServiceEndpoint(host: "192.0.2.1", port: 47321, pid: 1, version: "test", startedAt: 0)
        XCTAssertThrowsError(try NotificationReceiptClient.url(endpoint: remote, path: "/healthz"))
    }

    func testClickCallsServicesWithEncodedBufferWithoutRetry() async throws {
        let buffer = "中文 & + % # \\ \"buffer\"\n😀"
        let navigation = SystemNotification.legacyNavigationURL(buffer: buffer)
        let stub = ReceiptStub(statuses: [200])
        let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) }, retryDelay: { _ in XCTFail("点击操作不自动重试") })
        try await client.activate(navigation)
        let request = try XCTUnwrap(stub.requests.last)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/emacs/activate")
        let query = try XCTUnwrap(URLComponents(url: XCTUnwrap(request.url), resolvingAgainstBaseURL: false))
        XCTAssertEqual(query.queryItems?.first?.value, buffer)
        XCTAssertFalse(query.percentEncodedQuery?.contains("+") ?? true)
        XCTAssertEqual(stub.requests.count, 2)
    }

    func testSlackStatusClickOpensTargetInSnapshotSession() async throws {
        let session = try XCTUnwrap(UUID(uuidString: "550e8400-e29b-41d4-a716-446655440000"))
        let conversation = SlackNewConversation(
            id: "TTEAM.CWORK", title: "产品讨论", teamName: "工作区",
            sessionID: session, callback: "/slack/conversations/TTEAM.CWORK/open?message_ts=200.2&thread_ts=200.1", unreadCount: 3
        )
        let stub = ReceiptStub(statuses: [200])
        let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) })
        try await client.openSlackConversation(conversation)
        let request = try XCTUnwrap(stub.requests.last)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/slack/conversations/TTEAM.CWORK/open")
        XCTAssertEqual(request.url?.query, "message_ts=200.2&thread_ts=200.1")
        XCTAssertEqual(request.value(forHTTPHeaderField: "X-Loki-Slack-Session"), session.uuidString)
        XCTAssertEqual(stub.requests.count, 2)
    }

    func testClickSurfacesMissingBufferAndRejectsExternalCallback() async throws {
        let stub = ReceiptStub(statuses: [404])
        let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) }, retryDelay: { _ in XCTFail("点击操作不自动重试") })
        do {
            try await client.activate(SystemNotification.legacyNavigationURL(buffer: "已关闭"))
            XCTFail("应报告目标失效")
        } catch NotificationActivationError.rejected(let status, let message) {
            XCTAssertEqual(status, 404)
            XCTAssertNil(message)
        }
        XCTAssertEqual(stub.requests.count, 2)
        for callback in ["https://example.com/activate", "//example.com/activate", "/\\example.com", "/%2Fexample.com"] {
            var components = URLComponents(string: "loki-assistant://notification/activate")!
            components.queryItems = [URLQueryItem(name: "callback", value: callback)]
            XCTAssertThrowsError(try NotificationReceiptClient.callbackPath(XCTUnwrap(components.url)))
        }
    }

    func testClickPreservesServicesFailureMessageWithoutRetry() async throws {
        for (status, message) in [(404, "目标 buffer 已不存在"), (503, "无法连接目标 Emacs，请确认 server 已启动"), (504, "Emacs 调用超时")] {
            let stub = ReceiptStub(statuses: [status], errorBody: try JSONSerialization.data(withJSONObject: ["api_version": "1", "status": status, "error": ["source": "business", "code": "test-error", "message": message]]))
            let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) }, retryDelay: { _ in XCTFail("点击操作不自动重试") })
            do {
                try await client.activate(SystemNotification.legacyNavigationURL(buffer: "旧通知"))
                XCTFail("应显示 services 返回的失败原因")
            } catch let error as NotificationActivationError {
                XCTAssertEqual(error.localizedDescription, message)
            }
            XCTAssertEqual(stub.requests.count, 2)
        }
    }

    func testClickDoesNotRetryOrPostWhenServicesIsUnavailable() async throws {
        let stub = ReceiptStub(statuses: [], healthBody: "{}")
        let client = NotificationReceiptClient(discover: { stub.discover() }, transport: { try stub.respond($0) }, retryDelay: { _ in XCTFail("点击操作不自动重试") })
        do {
            try await client.activate(SystemNotification.legacyNavigationURL(buffer: "旧通知"))
            XCTFail("services 不可用时应提示失败")
        } catch NotificationReceiptError.serviceUnavailable {}
        XCTAssertEqual(stub.requests.count, 1)
    }

    private func notificationURL(_ payload: String) throws -> URL {
        var components = URLComponents()
        components.scheme = "loki-assistant"
        components.host = "emacs"
        components.path = "/notification"
        components.queryItems = [URLQueryItem(name: "payload", value: payload)]
        return try XCTUnwrap(components.url)
    }
}

private final class ReceiptStub: @unchecked Sendable {
    private let lock = NSLock()
    private var statuses: [Int]
    private var recorded: [URLRequest] = []
    private var discoveries = 0
    private let healthBody: String
    private let errorBody: Data?

    init(statuses: [Int], healthBody: String = #"{"service":"loki-services","status":"ok","version":"test","uptime_seconds":0}"#, errorBody: Data? = nil) {
        self.statuses = statuses
        self.healthBody = healthBody
        self.errorBody = errorBody
    }
    var requests: [URLRequest] { lock.withLock { recorded } }

    func discover() -> ServiceEndpoint {
        lock.withLock {
            defer { discoveries += 1 }
            return ServiceEndpoint(host: "127.0.0.1", port: UInt16(47321 + discoveries), pid: 1, version: "test", startedAt: 0)
        }
    }

    func respond(_ request: URLRequest) throws -> (Data, URLResponse) {
        try lock.withLock {
            recorded.append(request)
            let status = request.url!.path == "/healthz" ? 200 : statuses.removeFirst()
            if status == -1 { throw URLError(.networkConnectionLost) }
            let data = request.url!.path == "/healthz" ? Data(healthBody.utf8) : errorBody ?? Data(healthBody.utf8)
            return (data, HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!)
        }
    }
}

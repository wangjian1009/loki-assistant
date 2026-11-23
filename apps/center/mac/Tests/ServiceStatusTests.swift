import XCTest
@testable import LokiAssistant

private actor MenuStatusTransport {
    private(set) var paths: [String] = []

    func send(_ request: URLRequest) throws -> (Data, URLResponse) {
        let url = try XCTUnwrap(request.url)
        paths.append(url.path)
        let alerts = paths.count == 1
            ? #"[{"id":"backup.disk-space","title":"备份磁盘空间不足","body":"剩余空间低于 10 GB","level":"warning","app":"backup"},{"id":"slack.reauthorize","title":"Slack 需要重新授权","body":null,"callback_url":"/slack/teams/TTEAM/reset-credentials"}]"#
            : "[]"
        let data = Data(#"{"api_version":"1","status":{"service":{"start_at":200},"alert":{"active":\#(alerts)},"slack":{"new_conversations":[{"id":"TTEAM.CWORK","title":"产品讨论","team_name":"工作区","unread_count":3,"session_id":"550e8400-e29b-41d4-a716-446655440000","callback":"/slack/conversations/TTEAM.CWORK/open?message_ts=200.1"}]}}}"#.utf8)
        return (data, try XCTUnwrap(HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)))
    }
}

final class ServiceStatusTests: XCTestCase {
    func testDecodesHealthResponse() throws {
        let data = Data(#"{"service":"loki-services","status":"ok","version":"0.1.0","uptime_seconds":42}"#.utf8)
        let response = try JSONDecoder().decode(HealthResponse.self, from: data)

        XCTAssertEqual(response.service, "loki-services")
        XCTAssertEqual(response.status, "ok")
        XCTAssertEqual(response.uptimeSeconds, 42)
    }

    func testDecodesServiceEndpoint() throws {
        let data = Data(#"{"host":"127.0.0.1","port":47321,"pid":42,"version":"0.1.0","started_at":100}"#.utf8)
        let endpoint = try JSONDecoder().decode(ServiceEndpoint.self, from: data)

        XCTAssertEqual(endpoint.host, "127.0.0.1")
        XCTAssertEqual(endpoint.port, 47321)
        XCTAssertEqual(endpoint.version, "0.1.0")
        XCTAssertEqual(endpoint.startedAt, 100)
    }

    func testDecodesFullStatusSnapshot() throws {
        let data = Data(#"{"api_version":"1","status":{"service":{"start_at":200},"network":{"my":{"status":"connected"}}},"updated_at":201}"#.utf8)
        let snapshot = try JSONDecoder().decode(ServiceStatusSnapshot.self, from: data)

        XCTAssertEqual(snapshot.apiVersion, "1")
        XCTAssertEqual(snapshot.status.service?.startedAt, 200)
        XCTAssertEqual(snapshot.status.network?.my?.status, "connected")
        XCTAssertEqual(snapshot.updatedAt, 201)
    }

    @MainActor
    func testStoppedDiscoveryDoesNotRequestStatus() async {
        let model = ServiceStatusModel(
            discover: { throw NotificationReceiptError.serviceUnavailable },
            transport: { _ in
                XCTFail("服务未持有锁时不应请求状态接口")
                throw URLError(.badURL)
            }
        )
        await model.refresh()
        XCTAssertEqual(model.status, .stopped)
        XCTAssertEqual(model.indicator, .red)
        XCTAssertNil(model.startedAt)
        XCTAssertNotNil(model.lastUpdated)
    }

    @MainActor
    func testStatusUsesDiscoveredEndpointAndStartTime() async {
        let model = ServiceStatusModel(
            discover: { ServiceEndpoint(host: "127.0.0.1", port: 45678, pid: 42, version: "0.1.0", startedAt: 100) },
            transport: { request in
                XCTAssertEqual(request.url?.absoluteString, "http://127.0.0.1:45678/status")
                let data = Data(#"{"api_version":"1","status":{"service":{"start_at":200},"network":{"my":{"status":"connected"}}},"updated_at":201}"#.utf8)
                return (data, HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: nil)!)
            }
        )
        await model.refresh()
        guard case let .healthy(version) = model.status else { return XCTFail("应显示健康状态") }
        XCTAssertEqual(version, "0.1.0")
        XCTAssertEqual(model.startedAt, Date(timeIntervalSince1970: 200))
        XCTAssertEqual(model.networkStatus, "connected")
        XCTAssertTrue(model.activeAlerts.isEmpty)
        XCTAssertEqual(model.snapshot?.updatedAt, 201)
        XCTAssertEqual(model.indicator, .green)
    }

    @MainActor
    func testMenuFetchesOneSnapshotPerOpening() async throws {
        let transport = MenuStatusTransport()
        let model = ServiceStatusModel(
            discover: { ServiceEndpoint(host: "127.0.0.1", port: 45678, pid: 42, version: "test", startedAt: 100) },
            transport: { try await transport.send($0) }
        )
        let beforeOpening = await transport.paths
        XCTAssertTrue(beforeOpening.isEmpty)
        defer { model.menuDidClose() }

        for count in 1...2 {
            let before = model.lastUpdated
            model.menuDidOpen()
            model.menuDidOpen()
            let deadline = Date().addingTimeInterval(3)
            while model.lastUpdated == before, Date() < deadline {
                try await Task.sleep(for: .milliseconds(10))
            }
            let paths = await transport.paths
            XCTAssertEqual(paths, Array(repeating: "/status", count: count))
            XCTAssertEqual(model.slackNewConversations.map(\.title), ["产品讨论"])
            XCTAssertEqual(model.slackNewConversations.first?.unreadCount, 3)
            if count == 1 {
                XCTAssertEqual(model.activeAlerts.map(\.id), ["backup.disk-space", "slack.reauthorize"])
                XCTAssertEqual(model.activeAlerts.map(\.menuTitle), ["备份磁盘空间不足 · 剩余空间低于 10 GB", "Slack 需要重新授权"])
                XCTAssertEqual(model.activeAlerts.map(\.callbackURL), [nil, "/slack/teams/TTEAM/reset-credentials"])
            } else {
                XCTAssertTrue(model.activeAlerts.isEmpty)
            }
            XCTAssertNotEqual(model.lastUpdated, before)
            model.menuDidClose()
        }
    }

    @MainActor
    func testNetworkIndicatorDistinguishesOfflineRegistrationAndTransportFailure() async {
        for (phase, expected) in [
            ("offline", ServiceIndicator.green),
            ("connected", .green),
            ("connecting", .yellow),
            ("failed", .yellow),
            ("disconnected", .yellow),
        ] {
            let model = ServiceStatusModel(
                discover: { ServiceEndpoint(host: "127.0.0.1", port: 45678, pid: 42, version: "test", startedAt: 100) },
                transport: { request in
                    XCTAssertEqual(request.url?.path, "/status")
                    let data = Data("{\"api_version\":\"1\",\"status\":{\"service\":{\"start_at\":200},\"network\":{\"my\":{\"status\":\"\(phase)\"}}},\"updated_at\":201}".utf8)
                    return (data, HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: nil)!)
                })
            await model.refresh()
            XCTAssertEqual(model.indicator, expected, phase)
        }
        let model = ServiceStatusModel(
            discover: { ServiceEndpoint(host: "127.0.0.1", port: 45678, pid: 42, version: "test", startedAt: 100) },
            transport: { _ in throw URLError(.cannotConnectToHost) })
        await model.refresh()
        XCTAssertEqual(model.indicator, .red)
        XCTAssertNil(model.snapshot)
    }
}

import XCTest
@testable import LokiAssistant

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

    @MainActor
    func testStoppedDiscoveryDoesNotRequestHealth() async {
        let model = ServiceStatusModel(
            discover: { throw NotificationReceiptError.serviceUnavailable },
            transport: { _ in
                XCTFail("服务未持有锁时不应请求 HTTP")
                throw URLError(.badURL)
            }
        )
        await model.refresh()
        XCTAssertEqual(model.status, .stopped)
        XCTAssertNil(model.startedAt)
        XCTAssertNotNil(model.lastUpdated)
    }

    @MainActor
    func testHealthStatusUsesDiscoveredEndpointAndStartTime() async {
        let model = ServiceStatusModel(
            discover: { ServiceEndpoint(host: "127.0.0.1", port: 45678, pid: 42, version: "0.1.0", startedAt: 100) },
            transport: { request in
                XCTAssertEqual(request.url?.absoluteString, "http://127.0.0.1:45678/healthz")
                let data = Data(#"{"service":"loki-services","status":"ok","version":"0.1.0","uptime_seconds":42}"#.utf8)
                return (data, HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: nil)!)
            }
        )
        await model.refresh()
        guard case let .healthy(health) = model.status else { return XCTFail("应显示健康状态") }
        XCTAssertEqual(health.version, "0.1.0")
        XCTAssertEqual(model.startedAt, Date(timeIntervalSince1970: 100))
    }
}

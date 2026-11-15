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

    func testHealthURLUsesLoopbackServicesEndpoint() {
        XCTAssertEqual(ServiceStatusModel.healthURL.absoluteString, "http://127.0.0.1:47321/healthz")
    }
}

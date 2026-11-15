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
}

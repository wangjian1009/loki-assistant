import Foundation
import XCTest
@testable import LokiAssistant

final class ServiceSettingsTests: XCTestCase {
    private static let endpoint = ServiceEndpoint(
        host: "127.0.0.1", port: 47321, pid: 1, version: "0.1.0", startedAt: 0)

    func testOpensConfigurationThroughAppAPI() async throws {
        let client = ServiceSettingsClient(discover: { Self.endpoint }, transport: { request in
            let url = try XCTUnwrap(request.url)
            let body: String
            if url.path == "/healthz" {
                body = #"{"service":"loki-services","status":"ok","version":"0.1.0","uptime_seconds":1}"#
            } else {
                XCTAssertEqual(url.path, "/emacs/apps/open")
                XCTAssertEqual(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems,
                               [URLQueryItem(name: "app", value: "config")])
                XCTAssertEqual(request.httpMethod, "POST")
                XCTAssertNil(request.httpBody)
                body = #"{"api_version":"1","app":"config","opened":true}"#
            }
            return (Data(body.utf8), HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!)
        })
        try await client.open()
    }

    func testPreservesMissingAppAndGatewayErrors() async {
        for (status, code, message) in [
            (404, "app-not-found", "Emacs 应用不存在或未安装：config"),
            (422, "lisp-error", "配置界面加载失败")
        ] {
            let client = ServiceSettingsClient(discover: { Self.endpoint }, transport: { request in
                let url = try XCTUnwrap(request.url)
                let health = url.path == "/healthz"
                let body = health
                    ? Data(#"{"service":"loki-services","status":"ok","version":"0.1.0","uptime_seconds":1}"#.utf8)
                    : try JSONEncoder().encode(["code": code, "message": message])
                return (body, HTTPURLResponse(url: url, statusCode: health ? 200 : status, httpVersion: nil, headerFields: nil)!)
            })
            do {
                try await client.open()
                XCTFail("应反馈服务端错误")
            } catch {
                XCTAssertEqual(error.localizedDescription, message)
            }
        }
    }

    func testFailedHealthDoesNotOpenSettings() async {
        let client = ServiceSettingsClient(discover: { Self.endpoint }, transport: { request in
            let url = try XCTUnwrap(request.url)
            XCTAssertEqual(url.path, "/healthz")
            return (Data(), HTTPURLResponse(url: url, statusCode: 503, httpVersion: nil, headerFields: nil)!)
        })
        do {
            try await client.open()
            XCTFail("健康检查失败时应停止")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("services"))
        }
    }
}

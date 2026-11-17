import Foundation
import XCTest
@testable import LokiAssistant

private actor StatusTestTransport {
    private(set) var paths: [String] = []
    private(set) var query: String?
    var failing = false
    var blocked = false
    private var statusBody: String?
    private var waiter: CheckedContinuation<Void, Never>?

    func setFailing() { failing = true }
    func setBlocked() { blocked = true }
    func setStatusBody(_ body: String) { statusBody = body }
    func release() { blocked = false; waiter?.resume(); waiter = nil }

    func send(_ request: URLRequest) async throws -> (Data, URLResponse) {
        let url = try XCTUnwrap(request.url)
        paths.append(url.path)
        let body: String
        var code = 200
        switch url.path {
        case "/healthz":
            body = #"{"service":"loki-services","status":"ok","version":"0.1.0","uptime_seconds":1}"#
        case "/emacs/status":
            XCTAssertEqual(request.httpMethod, "GET")
            if blocked { await withCheckedContinuation { waiter = $0 } }
            if failing {
                code = 404
                body = #"{"code":"status-not-found","message":"尚未收到 buffer 上报"}"#
            } else if let statusBody {
                body = statusBody
            } else {
                body = #"{"api_version":"1","updated_at":123,"status":{"emacs":{"buffer":[{"id":"token:1","name":"中文会话","mode":"agent-shell-mode","directory":"/tmp/中文/","file":null,"modified":false}]}}}"#
            }
        case "/emacs/activate":
            XCTAssertEqual(request.httpMethod, "POST")
            query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first?.value
            body = #"{"api_version":"1","activation":"emacs","buffer":null,"buffer_id":"token:1"}"#
        default:
            XCTFail("不应请求 \(url.path)")
            body = "{}"
        }
        return (Data(body.utf8), HTTPURLResponse(url: url, statusCode: code, httpVersion: nil, headerFields: nil)!)
    }
}

final class EmacsReportedStatusTests: XCTestCase {
    private static func client(_ transport: StatusTestTransport) -> EmacsReportedStatusClient {
        EmacsReportedStatusClient(
            discover: { ServiceEndpoint(host: "127.0.0.1", port: 12345, pid: 1, version: "0.1.0", startedAt: 0) },
            transport: { try await transport.send($0) })
    }

    @MainActor
    private func waitUntil(_ condition: () async -> Bool) async throws {
        let deadline = Date().addingTimeInterval(3)
        while !(await condition()) && Date() < deadline { await Task.yield() }
        let finished = await condition()
        XCTAssertTrue(finished, "异步状态未在期限内完成")
    }

    func testFetchesReportedBuffersAndActivatesOnlyThroughServices() async throws {
        let transport = StatusTestTransport()
        let client = Self.client(transport)
        let snapshot = try await client.fetchBuffers()
        XCTAssertEqual(snapshot.bufferGroups.first?.buffers.first?.name, "中文会话")
        XCTAssertEqual(snapshot.updatedAt, 123)
        try await client.activate(XCTUnwrap(snapshot.bufferGroups.first?.buffers.first))
        let paths = await transport.paths
        let query = await transport.query
        XCTAssertEqual(paths, ["/healthz", "/emacs/status", "/healthz", "/emacs/activate"])
        XCTAssertEqual(query, "token:1")
    }

    func testPreservesMissingReportError() async {
        let transport = StatusTestTransport()
        await transport.setFailing()
        do {
            _ = try await Self.client(transport).fetchBuffers()
            XCTFail("应返回服务端错误")
        } catch { XCTAssertEqual(error.localizedDescription, "尚未收到 buffer 上报") }
    }

    @MainActor
    func testGroupsBuffersByAppAndOmitsAppsWithoutBuffers() async throws {
        let transport = StatusTestTransport()
        await transport.setStatusBody(#"""
        {"api_version":"1","updated_at":123,"status":{
          "tools":{"buffer":[{"id":"token:1","name":"工具","mode":"fundamental-mode","directory":"/tmp/","file":null,"modified":false}]},
          "emacs":{"runtime":{"pid":42},"buffer":[{"id":"token:1","name":"中文会话","mode":"agent-shell-mode","directory":"/tmp/中文/","file":null,"modified":false}]},
          "empty":{"buffer":[]},"runtime-only":{"runtime":{"pid":42}}
        }}
        """#)
        let model = EmacsReportedStatusModel(client: Self.client(transport))
        model.menuDidOpen()
        try await waitUntil { model.hasSnapshot }
        XCTAssertEqual(model.bufferGroups.map(\.app), ["emacs", "tools"])
        XCTAssertEqual(model.bufferGroups.map { $0.buffers.map(\.name) }, [["中文会话"], ["工具"]])
        model.menuDidClose()
    }

    func testEmptyStatusReturnsNoGroups() async throws {
        let transport = StatusTestTransport()
        await transport.setStatusBody(#"{"api_version":"1","status":{},"updated_at":null}"#)
        let snapshot = try await Self.client(transport).fetchBuffers()
        XCTAssertTrue(snapshot.bufferGroups.isEmpty)
        XCTAssertNil(snapshot.updatedAt)
    }

    func testRejectsDuplicateBufferIDsWithinApp() async throws {
        let transport = StatusTestTransport()
        await transport.setStatusBody(#"""
        {"api_version":"1","status":{"emacs":{"buffer":[
          {"id":"same","name":"一","mode":"fundamental-mode","directory":"/tmp/","modified":false},
          {"id":"same","name":"二","mode":"fundamental-mode","directory":"/tmp/","modified":false}
        ]}},"updated_at":123}
        """#)
        do {
            _ = try await Self.client(transport).fetchBuffers()
            XCTFail("同一 app 内重复的 buffer ID 应被拒绝")
        } catch {
            XCTAssertEqual(error.localizedDescription, "Emacs buffer 状态响应无效")
        }
    }

    @MainActor
    func testOnlyRefreshesOnMenuOpenAndRetainsPreviousSnapshotOnFailure() async throws {
        let transport = StatusTestTransport()
        let model = EmacsReportedStatusModel(client: Self.client(transport))
        model.refresh()
        let before = await transport.paths
        XCTAssertTrue(before.isEmpty)
        model.menuDidOpen()
        model.menuDidOpen()
        try await waitUntil { model.hasSnapshot }
        model.menuDidClose()
        XCTAssertEqual(model.bufferGroups.first?.buffers.count, 1)
        model.refresh()
        let closedPaths = await transport.paths
        XCTAssertEqual(closedPaths.count, 2)
        await transport.setFailing()
        model.menuDidOpen()
        try await waitUntil { model.refreshError != nil }
        XCTAssertEqual(model.bufferGroups.first?.buffers.first?.name, "中文会话")
        XCTAssertEqual(model.refreshError, "尚未收到 buffer 上报")
        model.menuDidClose()
    }

    @MainActor
    func testClosingMenuCancelsRefreshAndIgnoresLateResult() async throws {
        let transport = StatusTestTransport()
        await transport.setBlocked()
        let model = EmacsReportedStatusModel(client: Self.client(transport))
        model.menuDidOpen()
        try await waitUntil { await transport.paths.contains("/emacs/status") }
        model.menuDidClose()
        await transport.release()
        model.menuDidOpen()
        try await waitUntil { model.hasSnapshot }
        XCTAssertEqual(model.bufferGroups.first?.buffers.count, 1)
        XCTAssertNil(model.refreshError)
        model.menuDidClose()
    }
}

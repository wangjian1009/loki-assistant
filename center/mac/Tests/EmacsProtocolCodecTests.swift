import Foundation
import XCTest
@testable import LokiAssistant

final class EmacsProtocolCodecTests: XCTestCase {
    func testRequestPreservesUserTextAsData() throws {
        let prompt = "中文、\"引号\"、反斜线\\与换行\n第二行"
        let request = EmacsRequest(id: "test-1", operation: "send-prompt", params: ["prompt": prompt])
        let expression = try EmacsProtocolCodec.expression(for: request)
        let base64 = try XCTUnwrap(expression.split(separator: "\"").dropFirst().first)
        let data = try XCTUnwrap(Data(base64Encoded: String(base64)))
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let params = try XCTUnwrap(object["params"] as? [String: String])
        XCTAssertEqual(params["prompt"], prompt)
        XCTAssertFalse(expression.contains(prompt))
    }

    func testResponseValidatesVersionAndRequestID() throws {
        let valid = try output([
            "version": 1, "id": "test-1", "ok": true,
            "result": ["emacs_version": "30.2", "plugin_version": "0.1.0"]
        ])
        let info = try EmacsProtocolCodec.decode(valid, requestID: "test-1", as: ConnectionInfo.self)
        XCTAssertEqual(info.emacsVersion, "30.2")
        XCTAssertThrowsError(try EmacsProtocolCodec.decode(valid, requestID: "other", as: ConnectionInfo.self))
        let unsupported = try output([
            "version": 2, "id": "test-1", "ok": true,
            "result": ["emacs_version": "30.2", "plugin_version": "0.1.0"]
        ])
        XCTAssertThrowsError(try EmacsProtocolCodec.decode(unsupported, requestID: "test-1", as: ConnectionInfo.self))
    }

    func testSessionListDecodes() throws {
        let response = try output([
            "version": 1, "id": "sessions-1", "ok": true,
            "result": ["sessions": [[
                "id": "session-1",
                "buffer_name": "Codex Agent @ project",
                "project_path": "/tmp/project",
                "status": "ready"
            ]]]
        ])
        let result = try EmacsProtocolCodec.decode(response, requestID: "sessions-1", as: SessionList.self)
        XCTAssertEqual(result.sessions.count, 1)
        XCTAssertEqual(result.sessions[0].id, "session-1")
        XCTAssertEqual(result.sessions[0].bufferName, "Codex Agent @ project")
        XCTAssertEqual(result.sessions[0].status, "ready")
    }

    func testSelectedSessionDecodes() throws {
        let response = try output([
            "version": 1, "id": "select-1", "ok": true,
            "result": ["session_id": "session-1", "emacs_pid": 1234]
        ])
        let result = try EmacsProtocolCodec.decode(
            response,
            requestID: "select-1",
            as: SelectedSession.self
        )
        XCTAssertEqual(result.sessionID, "session-1")
        XCTAssertEqual(result.emacsPID, 1234)
        let invalidPayloads: [[String: Any]] = [
            ["session_id": "session-1"],
            ["session_id": "session-1", "emacs_pid": 0],
            ["session_id": "session-1", "emacs_pid": -1]
        ]
        for payload in invalidPayloads {
            let invalid = try output([
                "version": 1, "id": "select-1", "ok": true, "result": payload
            ])
            XCTAssertThrowsError(try EmacsProtocolCodec.decode(
                invalid, requestID: "select-1", as: SelectedSession.self
            ))
        }
    }

    func testAcceptedPromptDecodes() throws {
        let response = try output([
            "version": 1, "id": "send-1", "ok": true,
            "result": ["session_id": "session-1", "accepted": true]
        ])
        let result = try EmacsProtocolCodec.decode(
            response,
            requestID: "send-1",
            as: AcceptedPrompt.self
        )
        XCTAssertEqual(result.sessionID, "session-1")
        XCTAssertTrue(result.accepted)
    }

    func testMalformedResponseAndRemoteError() throws {
        XCTAssertThrowsError(try EmacsProtocolCodec.decode(
            Data("\"not base64\"\n".utf8), requestID: "test-1", as: ConnectionInfo.self
        ))
        let failed = try output([
            "version": 1, "id": "test-1", "ok": false,
            "error": ["code": "unknown-operation", "message": "未知操作"]
        ])
        XCTAssertThrowsError(try EmacsProtocolCodec.decode(failed, requestID: "test-1", as: ConnectionInfo.self)) { error in
            guard case let EmacsClientError.remote(code, message) = error else {
                return XCTFail("Expected a remote error, got \(error)")
            }
            XCTAssertEqual(code, "unknown-operation")
            XCTAssertEqual(message, "未知操作")
        }
    }

    private func output(_ object: [String: Any]) throws -> Data {
        let encoded = try JSONSerialization.data(withJSONObject: object).base64EncodedString()
        return Data("\"\(encoded)\"\n".utf8)
    }
}

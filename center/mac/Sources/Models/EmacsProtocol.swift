import Foundation

struct EmacsRequest: Encodable, Sendable {
    let version = 1
    let id: String
    let operation: String
    var params: [String: String] = [:]
}

struct ConnectionInfo: Decodable, Sendable {
    let emacsVersion: String
    let pluginVersion: String

    enum CodingKeys: String, CodingKey {
        case emacsVersion = "emacs_version"
        case pluginVersion = "plugin_version"
    }
}

struct AgentSession: Decodable, Identifiable, Sendable {
    let id: String
    let bufferName: String
    let projectPath: String?
    let status: String

    enum CodingKeys: String, CodingKey {
        case id
        case bufferName = "buffer_name"
        case projectPath = "project_path"
        case status
    }
}

struct SessionList: Decodable, Sendable {
    let sessions: [AgentSession]
}

struct SelectedSession: Decodable, Sendable {
    let sessionID: String

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
    }
}

struct AcceptedPrompt: Decodable, Sendable {
    let sessionID: String
    let accepted: Bool

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case accepted
    }
}

struct EmacsResponse<Result: Decodable & Sendable>: Decodable, Sendable {
    let version: Int
    let id: String?
    let ok: Bool
    let result: Result?
    let error: RemoteError?

    struct RemoteError: Decodable, Sendable {
        let code: String
        let message: String
    }
}

enum EmacsClientError: LocalizedError {
    case invalidResponse
    case remote(code: String, message: String)
    case executableNotFound
    case processFailed(String)
    case timedOut

    var errorDescription: String? {
        switch self {
        case .invalidResponse:
            "Emacs 返回的数据无效，请检查插件版本。"
        case let .remote(code, message):
            "\(message)（\(code)）"
        case .executableNotFound:
            "找不到 emacsclient，请在设置中检查路径。"
        case let .processFailed(message):
            "连接 Emacs 失败：\(message)"
        case .timedOut:
            "连接 Emacs 超时，请检查 server 是否可用。"
        }
    }
}

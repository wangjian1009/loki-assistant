import Foundation
import Observation

enum JSONValue: Codable, Equatable, Sendable {
    case string(String)
    case integer(Int)
    case number(Double)
    case boolean(Bool)
    case object([String: JSONValue])
    case array([JSONValue])
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .boolean(value)
        } else if let value = try? container.decode(Int.self) {
            self = .integer(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([String: JSONValue].self) {
            self = .object(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else {
            throw DecodingError.dataCorruptedError(in: container, debugDescription: "不支持的 JSON 值")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case let .string(value): try container.encode(value)
        case let .integer(value): try container.encode(value)
        case let .number(value): try container.encode(value)
        case let .boolean(value): try container.encode(value)
        case let .object(value): try container.encode(value)
        case let .array(value): try container.encode(value)
        case .null: try container.encodeNil()
        }
    }

    var text: String {
        switch self {
        case let .string(value): return value
        case let .integer(value): return String(value)
        case let .number(value): return String(value)
        case let .boolean(value): return String(value)
        case .object, .array:
            guard let data = try? JSONEncoder().encode(self) else { return "{}" }
            return String(data: data, encoding: .utf8) ?? "{}"
        case .null: return ""
        }
    }

    static func parse(_ text: String, type: ConfigDataType) -> JSONValue? {
        switch type {
        case .string, .filePath, .executable: return .string(text)
        case .integer: return Int(text).map(JSONValue.integer)
        case .number: return Double(text).map(JSONValue.number)
        case .boolean: return Bool(text).map(JSONValue.boolean)
        case .dictionary, .list:
            guard let data = text.data(using: .utf8) else { return nil }
            return try? JSONDecoder().decode(JSONValue.self, from: data)
        }
    }
}

enum ConfigDataType: String, Codable, Sendable {
    case string
    case integer
    case number
    case boolean
    case dictionary
    case list
    case filePath = "file_path"
    case executable
}

enum ConfigValueSource: String, Codable, Sendable {
    case user
    case `default`

    var displayName: String {
        switch self {
        case .user: "用户设置"
        case .default: "默认值"
        }
    }
}

struct ConfigValue: Codable, Equatable, Sendable {
    let value: JSONValue
    let source: ConfigValueSource
}

struct FilePathMetadata: Codable, Sendable {
    let `default`: String?
    let mustExist: Bool
    let directory: Bool

    enum CodingKeys: String, CodingKey {
        case `default`, directory
        case mustExist = "must_exist"
    }
}

struct ExecutableMetadata: Codable, Sendable {
    let defaultProgram: String
    let searchPaths: [String]

    enum CodingKeys: String, CodingKey {
        case defaultProgram = "default_program"
        case searchPaths = "search_paths"
    }
}

struct ConfigItemMetadata: Codable, Identifiable, Sendable {
    let key: String
    let name: String
    let description: String
    let dataType: ConfigDataType
    let required: Bool
    let `default`: JSONValue?
    let filePath: FilePathMetadata?
    let executable: ExecutableMetadata?

    var id: String { key }

    enum CodingKeys: String, CodingKey {
        case key, name, description
        case dataType = "data_type"
        case required
        case `default`
        case filePath = "file_path"
        case executable
    }
}

struct ConfigModuleMetadata: Codable, Identifiable, Sendable {
    let id: String
    let name: String
    let description: String
    let version: String
    let items: [ConfigItemMetadata]
}

struct MetaResponse: Codable, Sendable {
    let modules: [ConfigModuleMetadata]
}

struct ServiceEndpoint: Codable, Sendable {
    let host: String
    let port: UInt16
    let pid: UInt32
    let version: String
    let startedAt: UInt64

    enum CodingKeys: String, CodingKey {
        case host, port, pid, version
        case startedAt = "started_at"
    }
}

struct ModuleDataResponse: Codable, Sendable {
    let moduleID: String
    let values: [String: ConfigValue]

    enum CodingKeys: String, CodingKey {
        case moduleID = "module_id"
        case values
    }
}

private struct ConfigValidationResponse: Decodable, Sendable {
    let valid: Bool
    let error: ConfigRequestError?
}

@MainActor
@Observable
final class ConfigEditorModel {
    nonisolated static let serviceFileURL = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: ".assistant/service")

    private(set) var modules: [ConfigModuleMetadata] = []
    private(set) var selectedModuleID: String?
    private var moduleValues: [String: [String: ConfigValue]] = [:]
    private(set) var message: String?
    private(set) var messageIsError = false
    private(set) var loading = false
    private(set) var serviceAvailable: Bool?
    private var selectionHistory: [String] = []
    private var historyIndex = -1
    private var saveErrors: [String: [String: (text: String, message: String)]] = [:]
    private(set) var loadRevision = 0
    @ObservationIgnored private let session: URLSession
    @ObservationIgnored private let endpointFileURL: URL

    init(session: URLSession = .shared, serviceFileURL: URL = ConfigEditorModel.serviceFileURL) {
        self.session = session
        self.endpointFileURL = serviceFileURL
    }

    var values: [String: ConfigValue] {
        guard let selectedModuleID else { return [:] }
        return moduleValues[selectedModuleID] ?? [:]
    }

    var canGoBack: Bool { historyIndex > 0 }
    var canGoForward: Bool { historyIndex >= 0 && historyIndex < selectionHistory.count - 1 }

    func load() async {
        guard !loading else { return }
        loading = true
        defer { loading = false }
        do {
            let response: MetaResponse = try await request(path: "/meta")
            let availableModules = response.modules.filter { !$0.items.isEmpty }.sorted { $0.id < $1.id }
            let loadedValues = try await withThrowingTaskGroup(of: ModuleDataResponse.self) { group in
                for module in availableModules {
                    group.addTask {
                        let data: ModuleDataResponse = try await self.request(path: "/data/\(module.id)")
                        guard data.moduleID == module.id else {
                            throw ConfigRequestError(message: "服务器返回的配置模块不匹配：\(module.id)")
                        }
                        return data
                    }
                }
                var values: [String: [String: ConfigValue]] = [:]
                for try await data in group {
                    values[data.moduleID] = data.values
                }
                return values
            }
            // 元数据和实例数据全部读取成功后一起展示，避免编辑旧模块的数据。
            modules = availableModules
            moduleValues = loadedValues
            saveErrors = [:]
            loadRevision += 1
            if !modules.contains(where: { $0.id == selectedModuleID }) {
                selectedModuleID = modules.first?.id
            }
            selectionHistory = selectedModuleID.map { [$0] } ?? []
            historyIndex = selectionHistory.isEmpty ? -1 : 0
            serviceAvailable = true
            message = nil
            messageIsError = false
        } catch {
            serviceAvailable = false
            message = error.localizedDescription
            messageIsError = true
        }
    }

    func select(moduleID: String) async {
        guard !loading, modules.contains(where: { $0.id == moduleID }) else { return }
        if historyIndex < 0 || selectionHistory[historyIndex] != moduleID {
            selectionHistory = Array(selectionHistory.prefix(historyIndex + 1))
            selectionHistory.append(moduleID)
            historyIndex = selectionHistory.count - 1
        }
        selectedModuleID = moduleID
        message = nil
        messageIsError = false
    }

    func goBack() async {
        guard canGoBack else { return }
        historyIndex -= 1
        selectedModuleID = selectionHistory[historyIndex]
        message = nil
        messageIsError = false
    }

    func goForward() async {
        guard canGoForward else { return }
        historyIndex += 1
        selectedModuleID = selectionHistory[historyIndex]
        message = nil
        messageIsError = false
    }

    func draft(for item: ConfigItemMetadata) -> String {
        values[item.key]?.value.text ?? item.default?.text ?? ""
    }

    func source(for item: ConfigItemMetadata) -> ConfigValueSource? {
        values[item.key]?.source ?? (item.default == nil ? nil : .default)
    }

    func saveError(for item: ConfigItemMetadata, text: String, moduleID: String) -> String? {
        guard let error = saveErrors[moduleID]?[item.key], error.text == text else { return nil }
        return error.message
    }

    func save(_ item: ConfigItemMetadata, text: String, moduleID: String? = nil) async {
        guard let moduleID = moduleID ?? selectedModuleID,
              modules.contains(where: { $0.id == moduleID && $0.items.contains(where: { $0.key == item.key }) }) else {
            message = "未找到配置项"
            messageIsError = true
            return
        }
        let value = JSONValue.parse(text, type: item.dataType) ?? .string(text)
        do {
            let response: ConfigValue = try await request(
                path: "/data/\(moduleID)/\(item.key)",
                method: "PUT",
                body: value
            )
            moduleValues[moduleID, default: [:]][item.key] = response
            saveErrors[moduleID]?[item.key] = nil
            if moduleID == "control-center", item.key == "center_auto_start",
               case let .boolean(enabled) = value {
                try await CenterAutostartManager.setEnabled(enabled)
            }
            if selectedModuleID == moduleID {
                message = "已保存"
                messageIsError = false
            }
        } catch {
            saveErrors[moduleID, default: [:]][item.key] = (text, error.localizedDescription)
            if selectedModuleID == moduleID {
                message = error.localizedDescription
                messageIsError = true
            }
        }
    }

    func validationError(for item: ConfigItemMetadata, text: String, moduleID: String) async -> String? {
        let value = JSONValue.parse(text, type: item.dataType) ?? .string(text)
        do {
            let response: ConfigValidationResponse = try await request(
                path: "/config/validate", method: "POST",
                body: .object(["module_id": .string(moduleID), "key": .string(item.key), "value": value])
            )
            return response.valid ? nil : (response.error?.message ?? "配置值无效")
        } catch {
            return error.localizedDescription
        }
    }

    private func request<Response: Decodable & Sendable>(
        path: String,
        method: String = "GET",
        body: JSONValue? = nil
    ) async throws -> Response {
        let serviceData = try Data(contentsOf: endpointFileURL)
        let endpoint = try JSONDecoder().decode(ServiceEndpoint.self, from: serviceData)
        var components = URLComponents()
        components.scheme = "http"
        components.host = endpoint.host.contains(":") ? "[\(endpoint.host)]" : endpoint.host
        components.port = Int(endpoint.port)
        guard let baseURL = components.url else {
            throw URLError(.badURL)
        }
        let url = baseURL.appending(path: path)
        var request = URLRequest(url: url)
        request.httpMethod = method
        if let body {
            request.httpBody = try JSONEncoder().encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await session.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse else {
            throw URLError(.badServerResponse)
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            throw (try? JSONDecoder().decode(ConfigRequestError.self, from: data))
                ?? ConfigRequestError(message: "配置接口返回 HTTP \(httpResponse.statusCode)")
        }
        return try JSONDecoder().decode(Response.self, from: data)
    }
}

private struct ConfigRequestError: Decodable, LocalizedError, Sendable {
    let message: String
    var errorDescription: String? { message }
}

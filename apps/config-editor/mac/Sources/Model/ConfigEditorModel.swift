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
        case .string, .filePath: return .string(text)
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
}

struct ConfigItemMetadata: Codable, Identifiable, Sendable {
    let key: String
    let name: String
    let description: String
    let dataType: ConfigDataType
    let required: Bool
    let `default`: JSONValue?

    var id: String { key }

    enum CodingKeys: String, CodingKey {
        case key, name, description
        case dataType = "data_type"
        case required
        case `default`
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
}

struct ModuleDataResponse: Codable, Sendable {
    let moduleID: String
    let values: [String: JSONValue]

    enum CodingKeys: String, CodingKey {
        case moduleID = "module_id"
        case values
    }
}

struct ValueResponse: Codable, Sendable {
    let value: JSONValue
}

@MainActor
@Observable
final class ConfigEditorModel {
    static let serviceFileURL = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: ".assistant/service")

    private(set) var modules: [ConfigModuleMetadata] = []
    private(set) var selectedModuleID: String?
    private(set) var values: [String: JSONValue] = [:]
    private(set) var message: String?
    private(set) var loading = false

    func load() async {
        loading = true
        defer { loading = false }
        do {
            let response: MetaResponse = try await request(path: "/meta")
            modules = response.modules
            if selectedModuleID == nil || !modules.contains(where: { $0.id == selectedModuleID }) {
                selectedModuleID = modules.first?.id
            }
            if let selectedModuleID {
                try await loadData(for: selectedModuleID)
            }
            message = nil
        } catch {
            message = error.localizedDescription
        }
    }

    func select(moduleID: String) async {
        selectedModuleID = moduleID
        do {
            try await loadData(for: moduleID)
            message = nil
        } catch {
            message = error.localizedDescription
        }
    }

    func draft(for item: ConfigItemMetadata) -> String {
        values[item.key]?.text ?? item.default?.text ?? ""
    }

    func save(_ item: ConfigItemMetadata, text: String) async {
        guard let value = JSONValue.parse(text, type: item.dataType),
              let moduleID = selectedModuleID else {
            message = "配置值格式无效"
            return
        }
        do {
            let _: ValueResponse = try await request(
                path: "/data/\(moduleID)/\(item.key)",
                method: "PUT",
                body: value
            )
            values[item.key] = value
            message = "已保存"
        } catch {
            message = error.localizedDescription
        }
    }

    private func loadData(for moduleID: String) async throws {
        let response: ModuleDataResponse = try await request(path: "/data/\(moduleID)")
        values = response.values
    }

    private func request<Response: Decodable>(
        path: String,
        method: String = "GET",
        body: JSONValue? = nil
    ) async throws -> Response {
        let serviceData = try Data(contentsOf: Self.serviceFileURL)
        let endpoint = try JSONDecoder().decode(ServiceEndpoint.self, from: serviceData)
        guard let baseURL = URL(string: "http://\(endpoint.host):\(endpoint.port)") else {
            throw URLError(.badURL)
        }
        let url = baseURL.appending(path: path)
        var request = URLRequest(url: url)
        request.httpMethod = method
        if let body {
            request.httpBody = try JSONEncoder().encode(body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse,
              (200..<300).contains(httpResponse.statusCode) else {
            throw URLError(.badServerResponse)
        }
        return try JSONDecoder().decode(Response.self, from: data)
    }
}

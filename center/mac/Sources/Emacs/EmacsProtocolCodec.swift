import Foundation

enum EmacsProtocolCodec {
    static func expression(for request: EmacsRequest) throws -> String {
        let encoded = try JSONEncoder().encode(request).base64EncodedString()
        return "\n  (loki-assistant-request \"\(encoded)\")\n"
    }

    static func decode<Result: Decodable & Sendable>(
        _ output: Data,
        requestID: String,
        as resultType: Result.Type
    ) throws -> Result {
        guard output.count <= 1_048_576,
              let text = String(data: output, encoding: .utf8),
              text.hasSuffix("\n") else {
            throw EmacsClientError.invalidResponse
        }
        let printed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard printed.hasPrefix("\""), printed.hasSuffix("\""), printed.count >= 2,
              let data = Data(base64Encoded: String(printed.dropFirst().dropLast())) else {
            throw EmacsClientError.invalidResponse
        }
        let response: EmacsResponse<Result>
        do {
            response = try JSONDecoder().decode(EmacsResponse<Result>.self, from: data)
        } catch {
            throw EmacsClientError.invalidResponse
        }
        guard response.version == 1, response.id == requestID else {
            throw EmacsClientError.invalidResponse
        }
        if response.ok, let result = response.result, response.error == nil {
            return result
        }
        if !response.ok, response.result == nil, let error = response.error {
            throw EmacsClientError.remote(code: error.code, message: error.message)
        }
        throw EmacsClientError.invalidResponse
    }
}

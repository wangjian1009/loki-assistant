import Foundation

struct ServiceAPIErrorResponse: Decodable {
    let apiVersion: String
    let status: Int
    let error: ServiceAPIError

    enum CodingKeys: String, CodingKey {
        case apiVersion = "api_version"
        case status, error
    }
}

struct ServiceAPIError: Decodable {
    let source: String
    let code: String
    let message: String
}

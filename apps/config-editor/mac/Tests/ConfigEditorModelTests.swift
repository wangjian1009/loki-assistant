import XCTest
@testable import LokiConfigEditor

final class ConfigEditorModelTests: XCTestCase {
    func testDecodesConfigMetadata() throws {
        let data = Data(#"{"id":"module_a","name":"模块 A","description":"描述","version":"0.1.0","items":[{"key":"enabled","name":"启用","description":"","data_type":"boolean","required":false,"default":true}]}"#.utf8)
        let module = try JSONDecoder().decode(ConfigModuleMetadata.self, from: data)

        XCTAssertEqual(module.id, "module_a")
        XCTAssertEqual(module.items.first?.dataType, .boolean)
        XCTAssertEqual(module.items.first?.default, .boolean(true))
    }

    func testParsesJSONDictionaryValue() {
        let value = JSONValue.parse("{\"host\":\"127.0.0.1\"}", type: .dictionary)
        XCTAssertEqual(value, .object(["host": .string("127.0.0.1")]))
    }

    func testDecodesServiceEndpoint() throws {
        let data = Data(#"{"host":"127.0.0.1","port":47321,"pid":42,"version":"0.1.0"}"#.utf8)
        let endpoint = try JSONDecoder().decode(ServiceEndpoint.self, from: data)
        XCTAssertEqual(endpoint.port, 47321)
    }
}

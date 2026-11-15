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
        let data = Data(#"{"host":"127.0.0.1","port":47321,"pid":42,"version":"0.1.0","started_at":100}"#.utf8)
        let endpoint = try JSONDecoder().decode(ServiceEndpoint.self, from: data)
        XCTAssertEqual(endpoint.port, 47321)
    }

    func testDecodesExecutableMetadataAndDiscoveryRules() throws {
        let data = Data(#"{"key":"emacsclient_path","name":"emacsclient 位置","description":"请选择可执行文件","data_type":"executable","required":false,"default":null,"file_path":{"default":null,"must_exist":true,"directory":false},"executable":{"default_program":"emacsclient","search_paths":["/opt/homebrew/bin"]}}"#.utf8)
        let item = try JSONDecoder().decode(ConfigItemMetadata.self, from: data)
        XCTAssertEqual(item.dataType, .executable)
        XCTAssertEqual(item.executable?.defaultProgram, "emacsclient")
        XCTAssertEqual(item.executable?.searchPaths, ["/opt/homebrew/bin"])
        XCTAssertEqual(item.filePath?.mustExist, true)
        XCTAssertEqual(item.filePath?.directory, false)
    }

    func testDecodesEffectiveValuesAndSources() throws {
        let data = Data(#"{"module_id":"test","values":{"program":{"value":"/opt/homebrew/bin/emacsclient","source":"default"},"enabled":{"value":false,"source":"user"},"count":{"value":0,"source":"default"}}}"#.utf8)
        let response = try JSONDecoder().decode(ModuleDataResponse.self, from: data)
        XCTAssertEqual(response.values["program"], ConfigValue(value: .string("/opt/homebrew/bin/emacsclient"), source: .default))
        XCTAssertEqual(response.values["enabled"], ConfigValue(value: .boolean(false), source: .user))
        XCTAssertEqual(response.values["count"], ConfigValue(value: .integer(0), source: .default))
        let written = try JSONDecoder().decode(ConfigValue.self, from: Data(#"{"module_id":"test","key":"program","value":"custom","source":"user"}"#.utf8))
        XCTAssertEqual(written.source, .user)
    }

    func testExecutableWritesRemainJSONStringValues() throws {
        let value = try XCTUnwrap(JSONValue.parse("/Applications/Emacs App/emacsclient", type: .executable))
        XCTAssertEqual(value, .string("/Applications/Emacs App/emacsclient"))
        let encoded = try JSONEncoder().encode(value)
        XCTAssertEqual(try JSONDecoder().decode(String.self, from: encoded), "/Applications/Emacs App/emacsclient")
    }

}

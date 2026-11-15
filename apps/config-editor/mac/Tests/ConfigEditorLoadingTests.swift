import Foundation
import XCTest
@testable import LokiConfigEditor

final class ConfigEditorLoadingTests: XCTestCase {
    @MainActor
    func testStartupLoadsMetadataAndAllModuleInstancesBeforeEditing() async throws {
        let fixture = try Fixture()
        defer { fixture.close() }
        let model = fixture.model()
        await model.load()

        XCTAssertEqual(model.serviceAvailable, true)
        XCTAssertFalse(model.loading)
        XCTAssertEqual(model.modules.map(\.id), ["alpha", "beta"])
        XCTAssertEqual(model.selectedModuleID, "alpha")
        let item = try XCTUnwrap(model.modules.first?.items.first)
        XCTAssertEqual(item.dataType, .executable)
        XCTAssertEqual(model.draft(for: item), "/default/program")
        XCTAssertEqual(model.source(for: item), .default)
        XCTAssertEqual(Set(fixture.server.requests.map { $0.url!.path }), ["/meta", "/data/alpha", "/data/beta"])

        await model.select(moduleID: "beta")
        let enabled = try XCTUnwrap(model.modules.last?.items.first)
        XCTAssertEqual(model.draft(for: enabled), "false")
        XCTAssertEqual(model.source(for: enabled), .user)
        await model.goBack()
        XCTAssertEqual(model.selectedModuleID, "alpha")
        await model.goForward()
        XCTAssertEqual(model.selectedModuleID, "beta")
        XCTAssertEqual(fixture.server.requests.count, 3)
    }

    @MainActor
    func testSaveUsesTheItemsModuleAndUpdatesItsInstanceFromServer() async throws {
        let fixture = try Fixture()
        defer { fixture.close() }
        let model = fixture.model()
        await model.load()
        let item = try XCTUnwrap(model.modules.first?.items.first)
        await model.select(moduleID: "beta")
        fixture.server.reply("PUT /data/alpha/program", body: #"{"value":"/user/program","source":"user"}"#)

        await model.save(item, text: "/user/program", moduleID: "alpha")
        XCTAssertEqual(model.selectedModuleID, "beta")
        XCTAssertEqual(model.values["enabled"], ConfigValue(value: .boolean(false), source: .user))
        let request = try XCTUnwrap(fixture.server.requests.last)
        XCTAssertEqual(request.httpMethod, "PUT")
        XCTAssertEqual(request.url?.path, "/data/alpha/program")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
        let body = try fixture.server.body(of: request)
        XCTAssertEqual(try JSONDecoder().decode(String.self, from: body), "/user/program")
        await model.select(moduleID: "alpha")
        XCTAssertEqual(model.draft(for: item), "/user/program")
        XCTAssertEqual(model.source(for: item), .user)

        fixture.server.reply("PUT /data/alpha/program", status: 400, body: #"{"code":"invalid-config","message":"请选择有效的程序"}"#)
        await model.save(item, text: "invalid")
        XCTAssertEqual(model.message, "请选择有效的程序")
        XCTAssertTrue(model.messageIsError)
        XCTAssertEqual(model.saveError(for: item, text: "invalid", moduleID: "alpha"), "请选择有效的程序")
        XCTAssertNil(model.saveError(for: item, text: "changed", moduleID: "alpha"))
        XCTAssertEqual(model.draft(for: item), "/user/program")
    }

    @MainActor
    func testFailedInstanceLoadShowsServerErrorAndCanRetry() async throws {
        let fixture = try Fixture()
        defer { fixture.close() }
        fixture.server.reply("GET /data/alpha", status: 500, body: #"{"message":"读取配置失败"}"#)
        let model = fixture.model()
        await model.load()
        XCTAssertEqual(model.serviceAvailable, false)
        XCTAssertEqual(model.message, "读取配置失败")
        XCTAssertTrue(model.modules.isEmpty)
        XCTAssertTrue(model.values.isEmpty)

        fixture.server.reply("GET /data/alpha", body: Fixture.alphaData)
        await model.load()
        XCTAssertEqual(model.serviceAvailable, true)
        XCTAssertNil(model.message)
        XCTAssertEqual(model.modules.count, 2)
    }

    @MainActor
    func testRejectsMismatchedModuleData() async throws {
        let fixture = try Fixture()
        defer { fixture.close() }
        fixture.server.reply("GET /data/alpha", body: #"{"module_id":"beta","values":{}}"#)
        let model = fixture.model()
        await model.load()
        XCTAssertEqual(model.serviceAvailable, false)
        XCTAssertTrue(model.modules.isEmpty)
        XCTAssertEqual(model.message, "服务器返回的配置模块不匹配：alpha")
    }

    @MainActor
    func testEmptyMetadataDoesNotCreateLocalConfigurationItems() async throws {
        let fixture = try Fixture()
        defer { fixture.close() }
        fixture.server.reply("GET /meta", body: #"{"modules":[]}"#)
        let model = fixture.model()
        await model.load()
        XCTAssertEqual(model.serviceAvailable, true)
        XCTAssertTrue(model.modules.isEmpty)
        XCTAssertTrue(model.values.isEmpty)
        XCTAssertNil(model.selectedModuleID)
        XCTAssertEqual(fixture.server.requests.count, 1)
    }

    @MainActor
    func testReadsIPv6EndpointFromServiceDiscoveryFile() async throws {
        let fixture = try Fixture(host: "::1")
        defer { fixture.close() }
        let model = fixture.model()
        await model.load()
        XCTAssertEqual(model.serviceAvailable, true)
        XCTAssertTrue(try XCTUnwrap(fixture.server.requests.first?.url?.absoluteString).hasPrefix("http://[::1]:"))
    }

    @MainActor
    func testValidationUsesServerResultWithoutChangingLoadedValues() async throws {
        let fixture = try Fixture()
        defer { fixture.close() }
        let model = fixture.model()
        await model.load()
        let item = try XCTUnwrap(model.modules.first?.items.first)
        let original = model.values
        fixture.server.reply("POST /config/validate", body: #"{"valid":false,"error":{"code":"invalid-config","message":"文件没有执行权限"}}"#)
        let error = await model.validationError(for: item, text: "/not/executable", moduleID: "alpha")
        XCTAssertEqual(error, "文件没有执行权限")
        XCTAssertEqual(model.values, original)
        let request = try XCTUnwrap(fixture.server.requests.last)
        XCTAssertEqual(request.url?.path, "/config/validate")
        let body = try JSONDecoder().decode([String: JSONValue].self, from: fixture.server.body(of: request))
        XCTAssertEqual(body, ["module_id": .string("alpha"), "key": .string("program"), "value": .string("/not/executable")])
        fixture.server.reply("POST /config/validate", body: #"{"valid":true,"value":"/server/program"}"#)
        let accepted = await model.validationError(for: item, text: "program", moduleID: "alpha")
        XCTAssertNil(accepted)
        XCTAssertEqual(model.values, original)
    }
}

private struct Fixture {
    static let alphaData = #"{"module_id":"alpha","values":{"program":{"value":"/default/program","source":"default"}}}"#
    let directory: URL
    let serviceFile: URL
    let session: URLSession
    let server: StubServer
    let port: UInt16

    init(host: String = "127.0.0.1") throws {
        server = StubServer()
        port = StubProtocol.registry.register(server)
        directory = FileManager.default.temporaryDirectory.appending(path: "loki-config-loading-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        serviceFile = directory.appending(path: "service")
        let endpoint = ServiceEndpoint(host: host, port: port, pid: 1, version: "test", startedAt: 0)
        try JSONEncoder().encode(endpoint).write(to: serviceFile)
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubProtocol.self]
        session = URLSession(configuration: configuration)
        server.reply("GET /meta", body: #"{"modules":[{"id":"config","name":"配置中心","description":"","version":"1","items":[]},{"id":"beta","name":"Beta","description":"","version":"1","items":[{"key":"enabled","name":"启用","description":"","data_type":"boolean","required":false,"default":true}]},{"id":"alpha","name":"Alpha","description":"","version":"1","items":[{"key":"program","name":"程序","description":"","data_type":"executable","required":false,"default":null,"executable":{"default_program":"program","search_paths":[]}}]}]}"#)
        server.reply("GET /data/alpha", body: Self.alphaData)
        server.reply("GET /data/beta", body: #"{"module_id":"beta","values":{"enabled":{"value":false,"source":"user"}}}"#)
    }

    @MainActor
    func model() -> ConfigEditorModel {
        ConfigEditorModel(session: session, serviceFileURL: serviceFile)
    }

    func close() {
        session.invalidateAndCancel()
        StubProtocol.registry.remove(port)
        try? FileManager.default.removeItem(at: directory)
    }
}

private final class StubServer: @unchecked Sendable {
    private let lock = NSLock()
    private var replies: [String: (Int, Data)] = [:]
    private var recorded: [URLRequest] = []

    var requests: [URLRequest] { lock.withLock { recorded } }

    func reply(_ route: String, status: Int = 200, body: String) {
        lock.withLock { replies[route] = (status, Data(body.utf8)) }
    }

    func respond(_ request: URLRequest) -> (Int, Data) {
        lock.withLock {
            recorded.append(request)
            return replies["\(request.httpMethod ?? "GET") \(request.url!.path)"]
                ?? (404, Data(#"{"message":"测试接口不存在"}"#.utf8))
        }
    }

    func body(of request: URLRequest) throws -> Data {
        if let data = request.httpBody { return data }
        let stream = try XCTUnwrap(request.httpBodyStream)
        stream.open()
        defer { stream.close() }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 1024)
        while stream.hasBytesAvailable {
            let count = stream.read(&buffer, maxLength: buffer.count)
            if count <= 0 { break }
            data.append(contentsOf: buffer.prefix(count))
        }
        return data
    }
}

private final class StubRegistry: @unchecked Sendable {
    private let lock = NSLock()
    private var nextPort: UInt16 = 49000
    private var servers: [UInt16: StubServer] = [:]

    func register(_ server: StubServer) -> UInt16 {
        lock.withLock {
            nextPort += 1
            servers[nextPort] = server
            return nextPort
        }
    }

    func server(_ port: UInt16) -> StubServer? { lock.withLock { servers[port] } }
    func remove(_ port: UInt16) { lock.withLock { _ = servers.removeValue(forKey: port) } }
}

private final class StubProtocol: URLProtocol, @unchecked Sendable {
    static let registry = StubRegistry()

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url, let port = url.port, let server = Self.registry.server(UInt16(port)) else {
            client?.urlProtocol(self, didFailWithError: URLError(.cannotConnectToHost))
            return
        }
        let (status, data) = server.respond(request)
        let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

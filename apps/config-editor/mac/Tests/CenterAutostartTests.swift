import Foundation
import XCTest
@testable import LokiConfigEditor

final class CenterAutostartTests: XCTestCase {
    func testEnablingRegisteredAgentUsesKickstart() async throws {
        let fixture = try Fixture()
        defer { fixture.close() }
        let runner = Runner(registered: true)
        try await fixture.apply(true, runner: runner)
        XCTAssertEqual(runner.calls.map { $0[0] }, ["enable", "print", "kickstart"])
        let plist = try XCTUnwrap(PropertyListSerialization.propertyList(from: Data(contentsOf: fixture.plist), format: nil) as? [String: Any])
        XCTAssertEqual(plist["RunAtLoad"] as? Bool, true)
        XCTAssertEqual(plist["LimitLoadToSessionType"] as? String, "Aqua")
        XCTAssertEqual(plist["ProgramArguments"] as? [String], [fixture.executable.path])
        XCTAssertNotNil(plist["StandardErrorPath"])
    }

    func testEnablingUnregisteredAgentBootstrapsIt() async throws {
        let fixture = try Fixture()
        defer { fixture.close() }
        let runner = Runner(registered: false)
        try await fixture.apply(true, runner: runner)
        XCTAssertEqual(runner.calls.map { $0[0] }, ["enable", "print", "bootstrap"])
    }

    func testDisablingUnregisteredAgentIsIdempotent() async throws {
        let fixture = try Fixture()
        defer { fixture.close() }
        let runner = Runner(registered: false)
        try Data().write(to: fixture.plist)
        try await fixture.apply(false, runner: runner)
        try await fixture.apply(false, runner: runner)
        XCTAssertEqual(runner.calls.map { $0[0] }, ["print", "print"])
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.plist.path))
    }
}

private struct Fixture {
    let directory: URL
    let plist: URL
    let executable: URL

    init() throws {
        directory = FileManager.default.temporaryDirectory.appending(path: "loki-autostart-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        plist = directory.appending(path: "center.plist")
        executable = directory.appending(path: "Loki & Assistant")
        try Data().write(to: executable)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: executable.path)
    }

    func apply(_ enabled: Bool, runner: Runner) async throws {
        try await CenterAutostartManager.apply(enabled, plistURL: plist, executableURL: executable,
                                              logDirectory: directory.appending(path: "logs"), domain: "gui/501", run: runner.run)
    }

    func close() { try? FileManager.default.removeItem(at: directory) }
}

private final class Runner: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [[String]] = []
    private let registered: Bool

    init(registered: Bool) { self.registered = registered }
    var calls: [[String]] { lock.withLock { recorded } }

    func run(_ arguments: [String]) async throws -> CenterLaunchctlResult {
        lock.withLock { recorded.append(arguments) }
        return CenterLaunchctlResult(status: arguments.first == "print" && !registered ? 113 : 0, output: "")
    }
}

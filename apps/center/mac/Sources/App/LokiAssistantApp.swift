import AppKit
import Darwin
import SwiftUI

private final class CenterInstanceLock {
    private let descriptor: Int32

    init?() {
        let directory = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: ".assistant")
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let path = directory.appending(path: "center.lock").path
        let descriptor = open(path, O_CREAT | O_RDWR, 0o600)
        guard descriptor >= 0, flock(descriptor, LOCK_EX | LOCK_NB) == 0 else {
            if descriptor >= 0 { close(descriptor) }
            return nil
        }
        self.descriptor = descriptor
    }

    deinit {
        flock(descriptor, LOCK_UN)
        close(descriptor)
    }
}

@MainActor
private final class MenuTrackingObserver {
    private var tokens: [NSObjectProtocol] = []

    init(status: EmacsReportedStatusModel) {
        let center = NotificationCenter.default
        tokens.append(center.addObserver(
            forName: NSMenu.didBeginTrackingNotification,
            object: nil,
            queue: .main
        ) { [weak status] _ in
            Task { @MainActor in status?.menuTrackingDidBegin() }
        })
        tokens.append(center.addObserver(
            forName: NSMenu.didEndTrackingNotification,
            object: nil,
            queue: .main
        ) { [weak status] _ in
            Task { @MainActor in status?.menuDidClose() }
        })
    }

    isolated deinit {
        let center = NotificationCenter.default
        tokens.forEach(center.removeObserver)
    }
}

@main
struct LokiAssistantApp: App {
    @NSApplicationDelegateAdaptor(CenterAppDelegate.self) private var appDelegate
    private let instanceLock: CenterInstanceLock?
    private let menuTrackingObserver: MenuTrackingObserver
    @State private var status: ServiceStatusModel
    @State private var reportedStatus: EmacsReportedStatusModel
    @State private var idleSleep: IdleSleepController

    private static let menuBarIcon: NSImage = {
        // Xcode 将独立 PNG 的两种倍率合并为 TIFF，须通过 Bundle 加载。
        let image = Bundle.main.image(forResource: NSImage.Name("loki-assistant-menu-v1"))
            ?? NSImage(systemSymbolName: "server.rack", accessibilityDescription: "Loki的助理")
            ?? NSImage(size: NSSize(width: 20, height: 20))
        image.isTemplate = true
        image.size = NSSize(width: 20, height: 20)
        return image
    }()

    private static let menuBarSleepIcon: NSImage = {
        let image = Bundle.main.image(forResource: NSImage.Name("loki-assistant-menu-sleep-v1"))
            ?? Self.menuBarIcon
        image.isTemplate = true
        image.size = NSSize(width: 20, height: 20)
        return image
    }()

    init() {
        let model = ServiceStatusModel()
        _status = State(initialValue: model)
        let reportedStatus = EmacsReportedStatusModel(status: model, logger: .shared)
        _reportedStatus = State(initialValue: reportedStatus)
        _idleSleep = State(initialValue: IdleSleepController())
        menuTrackingObserver = MenuTrackingObserver(status: reportedStatus)
        // XCTest 的宿主进程不参与日常实例锁，也不轮询真实 services。
        let isTesting = ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
            || NSClassFromString("XCTestCase") != nil
        if isTesting {
            instanceLock = nil
            return
        }
        let instanceLock = CenterInstanceLock()
        self.instanceLock = instanceLock
        if instanceLock == nil {
            let existing = NSRunningApplication.runningApplications(
                withBundleIdentifier: Bundle.main.bundleIdentifier ?? "cc.loki-assistant.center"
            ).first { $0.processIdentifier != getpid() }
            existing?.activate(options: [.activateIgnoringOtherApps, .activateAllWindows])
            DispatchQueue.main.async {
                NSApplication.shared.terminate(nil)
            }
        }
    }

    var body: some Scene {
        MenuBarExtra {
            DashboardView(status: status, reportedStatus: reportedStatus, idleSleep: idleSleep)
        } label: {
            Image(nsImage: idleSleep.isEnabled ? Self.menuBarSleepIcon : Self.menuBarIcon)
            .accessibilityLabel(idleSleep.isEnabled ? "Loki的助理，已阻止空闲休眠" : "Loki的助理")
            .help(idleSleep.isEnabled ? "Loki的助理：已阻止空闲休眠" : "Loki的助理")
        }
        .menuBarExtraStyle(.menu)
    }
}

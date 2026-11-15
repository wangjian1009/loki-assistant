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

@main
struct LokiAssistantApp: App {
    @NSApplicationDelegateAdaptor(CenterAppDelegate.self) private var appDelegate
    private let instanceLock: CenterInstanceLock?
    @State private var status: ServiceStatusModel

    init() {
        let instanceLock = CenterInstanceLock()
        self.instanceLock = instanceLock
        let model = ServiceStatusModel()
        _status = State(initialValue: model)
        if instanceLock == nil {
            let existing = NSRunningApplication.runningApplications(
                withBundleIdentifier: Bundle.main.bundleIdentifier ?? "cc.loki-assistant.center"
            ).first { $0.processIdentifier != getpid() }
            existing?.activate(options: [.activateIgnoringOtherApps, .activateAllWindows])
            DispatchQueue.main.async {
                NSApplication.shared.terminate(nil)
            }
        } else {
            model.startMonitoring()
        }
    }

    var body: some Scene {
        MenuBarExtra("Loki Assistant", systemImage: "server.rack") {
            DashboardView(status: status)
                .onAppear {
                    Task {
                        await status.refresh()
                    }
                }
        }
        .menuBarExtraStyle(.menu)
    }
}

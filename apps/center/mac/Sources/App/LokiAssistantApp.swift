import AppKit
import SwiftUI

@MainActor
private final class MenuTrackingObserver {
    private var tokens: [NSObjectProtocol] = []

    init(status: ServiceStatusModel) {
        let center = NotificationCenter.default
        tokens.append(center.addObserver(
            forName: NSMenu.didBeginTrackingNotification,
            object: nil,
            queue: .main
        ) { [weak status] _ in
            MainActor.assumeIsolated { status?.menuDidOpen() }
        })
        tokens.append(center.addObserver(
            forName: NSMenu.didEndTrackingNotification,
            object: nil,
            queue: .main
        ) { [weak status] _ in
            MainActor.assumeIsolated { status?.menuDidClose() }
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
    private let menuTrackingObserver: MenuTrackingObserver
    private let mailSyncWindow = MailSyncWindow()
    @State private var status: ServiceStatusModel
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
        let model = ServiceStatusModel(logger: .shared)
        _status = State(initialValue: model)
        _idleSleep = State(initialValue: IdleSleepController())
        menuTrackingObserver = MenuTrackingObserver(status: model)
    }

    var body: some Scene {
        MenuBarExtra {
            DashboardView(
                status: status, idleSleep: idleSleep, openAlert: appDelegate.openAlert,
                openMailInbox: appDelegate.openMailInbox, syncMail: mailSyncWindow.show
            )
        } label: {
            Image(nsImage: idleSleep.isEnabled ? Self.menuBarSleepIcon : Self.menuBarIcon)
            .accessibilityLabel(idleSleep.isEnabled ? "Loki的助理，已阻止空闲休眠" : "Loki的助理")
            .help(idleSleep.isEnabled ? "Loki的助理：已阻止空闲休眠" : "Loki的助理")
        }
        .menuBarExtraStyle(.menu)
    }
}

import AppKit
import SwiftUI

struct SettingsButton: NSViewRepresentable {
    @Environment(\.openSettings) private var openSettings

    func makeNSView(context: Context) -> NSButton {
        let button = NSButton(
            title: "设置…", target: context.coordinator,
            action: #selector(Coordinator.showSettings(_:))
        )
        button.bezelStyle = .rounded
        button.identifier = NSUserInterfaceItemIdentifier("open-settings")
        button.setAccessibilityIdentifier("open-settings")
        return button
    }

    func updateNSView(_ button: NSButton, context: Context) {
        context.coordinator.openSettings = { openSettings() }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(openSettings: { openSettings() })
    }

    @MainActor
    final class Coordinator: NSObject {
        var openSettings: () -> Void

        init(openSettings: @escaping () -> Void) {
            self.openSettings = openSettings
        }

        @objc func showSettings(_ sender: NSButton) {
            SettingsWindowPresenter.shared.show(from: sender.window, openSettings: openSettings)
        }
    }
}

@MainActor
final class SettingsWindowPresenter {
    static let shared = SettingsWindowPresenter()
    private(set) weak var window: NSWindow?
    private var isRequested = false

    func show(from sourceWindow: NSWindow?, openSettings: () -> Void) {
        isRequested = true
        if sourceWindow !== window {
            sourceWindow?.orderOut(nil)
        }
        openSettings()
        scheduleActivation()
    }

    func attach(_ window: NSWindow) {
        self.window = window
        scheduleActivation()
    }

    private func scheduleActivation() {
        // 等待菜单栏面板处理完本次点击，再将原生设置窗口置前。
        DispatchQueue.main.async { [weak self] in
            guard let self, self.isRequested, let window = self.window else { return }
            self.isRequested = false
            if window.isMiniaturized { window.deminiaturize(nil) }
            NSRunningApplication.current.activate()
            window.makeKeyAndOrderFront(nil)
            window.orderFrontRegardless()
        }
    }
}

private struct SettingsWindowAccessor: NSViewRepresentable {
    func makeNSView(context: Context) -> WindowView { WindowView() }
    func updateNSView(_ view: WindowView, context: Context) {}

    final class WindowView: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let window {
                SettingsWindowPresenter.shared.attach(window)
            }
        }
    }
}

struct SettingsView: View {
    let model: ApplicationModel

    @AppStorage(EmacsConfiguration.executablePathKey)
    private var executablePath = EmacsConfiguration.defaultExecutablePath
    @AppStorage(EmacsConfiguration.socketNameKey)
    private var socketName = ""

    private var configuration: EmacsConfiguration {
        EmacsConfiguration(executablePath: executablePath, socketName: socketName)
    }

    var body: some View {
        Form {
            TextField("emacsclient 路径", text: $executablePath)
            TextField("Server 名称或 socket 路径", text: $socketName)
            Text("Server 留空时连接默认 Emacs server。")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .textFieldStyle(.roundedBorder)
        .padding(24)
        .frame(width: 540)
        .background { SettingsWindowAccessor() }
        .onChange(of: configuration, initial: true) { _, configuration in
            model.updateConfiguration(configuration)
        }
    }
}

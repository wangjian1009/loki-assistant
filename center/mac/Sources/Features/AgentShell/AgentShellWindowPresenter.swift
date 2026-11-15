import AppKit
import SwiftUI

@MainActor
final class AgentShellWindowPresenter {
    static let shared = AgentShellWindowPresenter()
    private(set) var window: NSWindow?

    func show(model: ApplicationModel, from sourceWindow: NSWindow?) {
        if sourceWindow !== window { sourceWindow?.orderOut(nil) }
        if window == nil {
            let window = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 1040, height: 720),
                styleMask: [.titled, .closable, .miniaturizable, .resizable],
                backing: .buffered, defer: false
            )
            window.title = "agent-shell 会话"
            window.identifier = NSUserInterfaceItemIdentifier("agent-shell-workspace")
            window.isReleasedWhenClosed = false
            window.contentMinSize = NSSize(width: 780, height: 560)
            window.contentView = NSHostingView(rootView: AgentShellView(model: model))
            window.center()
            window.setFrameAutosaveName("AgentShellWorkspace")
            self.window = window
        }
        // 菜单栏点击处理完成后再激活，避免面板抢回焦点。
        DispatchQueue.main.async { [weak self] in
            guard let window = self?.window else { return }
            if window.isMiniaturized { window.deminiaturize(nil) }
            NSApplication.shared.activate(ignoringOtherApps: true)
            window.makeKeyAndOrderFront(nil)
            window.orderFrontRegardless()
        }
    }
}

struct AgentShellWindowButton: NSViewRepresentable {
    let model: ApplicationModel

    func makeNSView(context: Context) -> NSButton {
        let button = NSButton(
            title: "打开会话管理", target: context.coordinator,
            action: #selector(Coordinator.showWorkspace(_:))
        )
        button.bezelStyle = .rounded
        button.image = NSImage(systemSymbolName: "sidebar.left", accessibilityDescription: nil)
        button.imagePosition = .imageLeading
        button.identifier = NSUserInterfaceItemIdentifier("open-agent-shell")
        button.setAccessibilityIdentifier("open-agent-shell")
        return button
    }

    func updateNSView(_ button: NSButton, context: Context) {
        context.coordinator.model = model
    }

    func makeCoordinator() -> Coordinator { Coordinator(model: model) }

    @MainActor
    final class Coordinator: NSObject {
        var model: ApplicationModel
        init(model: ApplicationModel) { self.model = model }

        @objc func showWorkspace(_ sender: NSButton) {
            AgentShellWindowPresenter.shared.show(model: model, from: sender.window)
        }
    }
}

import AppKit
import SwiftUI

@main
struct LokiAssistantApp: App {
    @State private var model = ApplicationModel()

    var body: some Scene {
        MenuBarExtra("Loki Assistant", systemImage: "terminal") {
            AgentShellView(model: model)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView(model: model)
        }
    }
}

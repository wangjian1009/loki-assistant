import SwiftUI

@main
struct LokiAssistantApp: App {
    @State private var status: ServiceStatusModel

    init() {
        let model = ServiceStatusModel()
        _status = State(initialValue: model)
        model.startMonitoring()
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

import AppKit
import SwiftUI

struct AgentShellView: View {
    let model: ApplicationModel

    @AppStorage(EmacsConfiguration.executablePathKey)
    private var executablePath = EmacsConfiguration.defaultExecutablePath
    @AppStorage(EmacsConfiguration.socketNameKey)
    private var socketName = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Loki Assistant")
                .font(.headline)

            Label(model.connectionMessage, systemImage: "terminal")
                .font(.callout)
                .textSelection(.enabled)

            Button("检查连接") {
                Task {
                    await model.checkConnection(configuration: EmacsConfiguration(
                        executablePath: executablePath,
                        socketName: socketName
                    ))
                }
            }
            .disabled(model.isCheckingConnection)

            Text("会话列表与需求发送将在后续版本提供。")
                .font(.caption)
                .foregroundStyle(.secondary)

            Divider()
            HStack {
                SettingsLink { Text("设置…") }
                Spacer()
                Button("退出") { NSApplication.shared.terminate(nil) }
            }
        }
        .padding(20)
        .frame(width: 360)
    }
}

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

            HStack {
                Text("agent-shell 会话")
                    .font(.headline)
                Spacer()
                Button {
                    Task {
                        await model.refreshSessions(configuration: EmacsConfiguration(
                            executablePath: executablePath,
                            socketName: socketName
                        ))
                    }
                } label: {
                    Label("刷新", systemImage: "arrow.clockwise")
                }
                .labelStyle(.iconOnly)
                .disabled(model.isLoadingSessions)
                .help("刷新会话列表")
            }

            if model.isLoadingSessions {
                ProgressView()
                    .controlSize(.small)
            } else if !model.sessions.isEmpty {
                List(model.sessions) { session in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(session.bufferName)
                            .font(.body)
                        HStack(spacing: 6) {
                            Text(session.status)
                            if let projectPath = session.projectPath {
                                Text(projectPath)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                        }
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 2)
                }
                .listStyle(.plain)
                .frame(maxHeight: 220)
            } else {
                Text(model.sessionMessage)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }

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

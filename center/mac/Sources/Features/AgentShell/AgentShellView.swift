import AppKit
import SwiftUI

struct AgentShellView: View {
    let model: ApplicationModel

    @AppStorage(EmacsConfiguration.executablePathKey)
    private var executablePath = EmacsConfiguration.defaultExecutablePath
    @AppStorage(EmacsConfiguration.socketNameKey)
    private var socketName = ""

    var body: some View {
        @Bindable var model = model

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
                    Button {
                        Task {
                            await model.selectSession(
                                sessionID: session.id,
                                configuration: EmacsConfiguration(
                                    executablePath: executablePath,
                                    socketName: socketName
                                )
                            )
                        }
                    } label: {
                        HStack {
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
                            Spacer()
                            Image(systemName: "checkmark")
                                .opacity(model.selectedSessionID == session.id ? 1 : 0)
                                .foregroundStyle(.tint)
                        }
                    }
                    .buttonStyle(.plain)
                    .contentShape(Rectangle())
                    .disabled(model.isSelectingSession)
                }
                .listStyle(.plain)
                .frame(maxHeight: 220)
            } else {
                Text(model.sessionMessage)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }

            Text("发送需求")
                .font(.headline)
            TextEditor(text: $model.promptText)
                .font(.body)
                .frame(minHeight: 72, maxHeight: 120)
                .overlay {
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(.quaternary)
                }
            Button("发送") {
                Task {
                    await model.sendPrompt(configuration: EmacsConfiguration(
                        executablePath: executablePath,
                        socketName: socketName
                    ))
                }
            }
            .disabled(model.isSendingPrompt || model.selectedSessionID == nil
                      || model.promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

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

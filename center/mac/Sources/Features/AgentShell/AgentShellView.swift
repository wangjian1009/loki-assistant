import AppKit
import Combine
import SwiftUI

struct AgentShellView: View {
    let model: ApplicationModel

    @AppStorage(EmacsConfiguration.executablePathKey)
    private var executablePath = EmacsConfiguration.defaultExecutablePath
    @AppStorage(EmacsConfiguration.socketNameKey)
    private var socketName = ""

    private var configuration: EmacsConfiguration {
        EmacsConfiguration(executablePath: executablePath, socketName: socketName)
    }

    private func statusText(_ status: String) -> String {
        switch status {
        case "ready": return "就绪"
        case "busy": return "运行中"
        default: return "状态未知"
        }
    }

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
                        executablePath: executablePath, socketName: socketName))
                }
            }
            .disabled(model.isCheckingConnection)

            HStack {
                Text("agent-shell 会话")
                    .font(.headline)
                if !model.sessions.isEmpty {
                    Text("\(model.sessions.count) 个")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    Task {
                        await model.refreshSessions(configuration: configuration)
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
                                configuration: configuration
                            )
                        }
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(session.bufferName)
                                    .font(.body)
                                HStack(spacing: 6) {
                                    Text(statusText(session.status))
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
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(model.isSelectingSession)
                }
                .listStyle(.plain)
                .frame(height: 220)
            } else {
                Text(model.sessionMessage)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }

            if !model.sessionMessage.isEmpty && !model.sessions.isEmpty {
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
                    await model.sendPrompt(configuration: configuration)
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
        .background {
            PanelVisibilityObserver {
                Task {
                    await model.refreshSessions(configuration: configuration)
                }
            }
        }
    }
}

private struct PanelVisibilityObserver: NSViewRepresentable {
    let onShow: @MainActor () -> Void

    func makeNSView(context: Context) -> VisibilityView {
        let view = VisibilityView()
        view.onShow = onShow
        return view
    }

    func updateNSView(_ view: VisibilityView, context: Context) {
        view.onShow = onShow
    }

    final class VisibilityView: NSView {
        var onShow: (@MainActor () -> Void)?
        private var subscription: AnyCancellable?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            subscription = nil
            guard let window else { return }
            // 面板内容可能被系统保留，重新显示窗口时仍需刷新会话。
            subscription = window.publisher(for: \.isVisible, options: [.initial, .new])
                .filter { $0 }
                .sink { [weak self] _ in
                    MainActor.assumeIsolated {
                        self?.onShow?()
                    }
                }
        }
    }
}

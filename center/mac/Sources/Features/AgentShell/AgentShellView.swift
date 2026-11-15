import SwiftUI

struct AgentShellView: View {
    let model: ApplicationModel
    @State private var searchText = ""
    @State private var statusFilter = "all"

    @AppStorage(EmacsConfiguration.executablePathKey)
    private var executablePath = EmacsConfiguration.defaultExecutablePath
    @AppStorage(EmacsConfiguration.socketNameKey)
    private var socketName = ""

    private var configuration: EmacsConfiguration {
        EmacsConfiguration(executablePath: executablePath, socketName: socketName)
    }

    private var selectedSession: AgentSession? {
        model.sessions.first { $0.id == model.selectedSessionID }
    }

    private var filteredSessions: [AgentSession] {
        let query = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        return model.sessions.filter {
            (statusFilter == "all" || $0.status == statusFilter)
                && (query.isEmpty || $0.bufferName.localizedStandardContains(query)
                    || ($0.projectPath?.localizedStandardContains(query) ?? false))
        }
    }

    var body: some View {
        HSplitView {
            sidebar
                .frame(minWidth: 240, idealWidth: 280, maxWidth: 360)
            workspace
                .frame(minWidth: 500, maxWidth: .infinity, maxHeight: .infinity)
        }
        .frame(minWidth: 780, minHeight: 560)
        .onChange(of: configuration, initial: true) { _, configuration in
            model.updateConfiguration(configuration)
        }
        .background {
            WindowVisibilityObserver {
                Task { await model.refreshStatus(configuration: configuration) }
            }
        }
    }

    private var sidebar: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 20) {
                HStack(spacing: 10) {
                    Image(systemName: "terminal.fill")
                        .font(.title2).foregroundStyle(.tint)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("会话空间").font(.headline)
                        Text("Loki Assistant").font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button {
                        Task { await model.refreshStatus(configuration: configuration) }
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .buttonStyle(.borderless)
                    .disabled(model.isLoadingSessions || model.isCheckingConnection)
                    .keyboardShortcut("r", modifiers: .command)
                    .help("刷新会话与连接状态（⌘R）")
                    .accessibilityLabel("刷新会话与连接状态")
                }

                HStack(spacing: 7) {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                    TextField("搜索会话或项目", text: $searchText)
                        .textFieldStyle(.plain)
                        .accessibilityIdentifier("session-search")
                    if !searchText.isEmpty {
                        Button { searchText = "" } label: {
                            Image(systemName: "xmark.circle.fill")
                        }
                        .buttonStyle(.plain).foregroundStyle(.secondary)
                        .accessibilityLabel("清除搜索")
                    }
                }
                .padding(9)
                .background(.background, in: RoundedRectangle(cornerRadius: 8))

                Picker("会话状态", selection: $statusFilter) {
                    Text("全部").tag("all")
                    Text("就绪").tag("ready")
                    Text("运行中").tag("busy")
                }
                .pickerStyle(.segmented).labelsHidden()
            }
            .padding(20)

            HStack {
                Text("会话").font(.caption.weight(.medium))
                Text("\(filteredSessions.count)").font(.caption).monospacedDigit()
                Spacer()
                if model.isLoadingSessions { ProgressView().controlSize(.mini) }
            }
            .foregroundStyle(.secondary)
            .padding(.horizontal, 22).padding(.bottom, 8)

            if filteredSessions.isEmpty {
                sidebarEmptyState
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(filteredSessions) { session in
                    Button {
                        model.focusSession(sessionID: session.id)
                    } label: {
                        sessionRow(session)
                    }
                    .buttonStyle(.plain)
                    .disabled(model.isSelectingSession || model.isSendingPrompt)
                    .listRowSeparator(.hidden)
                    .listRowInsets(EdgeInsets(top: 3, leading: 10, bottom: 3, trailing: 10))
                    .accessibilityIdentifier("session-\(session.id)")
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
                .accessibilityLabel("agent-shell 会话列表")
            }

            Divider()
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 6) {
                    Circle()
                        .fill(model.connectionState == .connected ? Color.teal : Color.secondary)
                        .frame(width: 6, height: 6)
                    Text(model.connectionState == .connected ? "Emacs 已连接" : "Emacs 未连接")
                        .font(.caption.weight(.medium))
                    Spacer()
                    SettingsButton(dismissSourceWindow: false).fixedSize()
                }
                Text(model.connectionMessage)
                    .font(.caption).foregroundStyle(.secondary)
                    .lineLimit(2).help(model.connectionMessage)
                Button("检查连接") {
                    Task { await model.checkConnection(configuration: configuration) }
                }
                .font(.caption).buttonStyle(.link)
                .disabled(model.isCheckingConnection)
            }
            .padding(16)
        }
        .background(Color(nsColor: .windowBackgroundColor))
    }

    private var sidebarEmptyState: some View {
        VStack(spacing: 10) {
            Image(systemName: model.sessionRefreshError == nil ? "bubble.left.and.bubble.right" : "wifi.exclamationmark")
                .font(.title2).foregroundStyle(.tertiary)
            Text(model.isLoadingSessions ? "正在加载会话…" :
                 (model.sessions.isEmpty ? "暂无可用会话" : "没有匹配的会话"))
                .font(.callout.weight(.medium))
            Text(model.sessions.isEmpty ?
                 (model.sessionRefreshError ?? "在 Emacs 中启动 agent-shell 后刷新。") : "试试其他名称、项目路径或状态。")
                .font(.caption).foregroundStyle(.secondary)
                .multilineTextAlignment(.center).textSelection(.enabled)
        }
        .padding(24)
    }

    private func sessionRow(_ session: AgentSession) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "bubble.left.and.text.bubble.right")
                .font(.system(size: 15))
                .foregroundStyle(model.selectedSessionID == session.id ? Color.accentColor : .secondary)
                .padding(.top, 2)
            VStack(alignment: .leading, spacing: 7) {
                Text(session.bufferName)
                    .font(.system(size: 13, weight: .medium)).lineLimit(1)
                Text(projectName(session))
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                SessionStatusBadge(status: session.status)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: 10)
                .fill(model.selectedSessionID == session.id ? Color.accentColor.opacity(0.10) : .clear)
        }
        .contentShape(Rectangle())
        .help(session.projectPath ?? session.bufferName)
        .accessibilityAddTraits(model.selectedSessionID == session.id ? [.isSelected] : [])
    }

    private var workspace: some View {
        VStack(spacing: 0) {
            workspaceHeader
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    if let session = selectedSession {
                        sessionOverview(session)
                    } else {
                        ContentUnavailableView {
                            Label("选择一个会话", systemImage: "sidebar.left")
                        } description: {
                            Text("从左侧选择 agent-shell 会话，查看状态并发送下一步需求。")
                        }
                        .frame(maxWidth: .infinity).padding(.top, 50)
                    }
                }
                .frame(maxWidth: 640, alignment: .leading)
                .padding(32)
                .frame(maxWidth: .infinity)
            }
            Spacer(minLength: 0)
            composer
        }
        .background(Color(nsColor: .textBackgroundColor))
    }

    private var workspaceHeader: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(selectedSession?.bufferName ?? "agent-shell")
                    .font(.headline).lineLimit(1)
                Text(selectedSession.map(projectName) ?? "你的本地会话工作空间")
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            if let session = selectedSession {
                Button {
                    Task {
                        await model.selectSession(sessionID: session.id, configuration: configuration)
                    }
                } label: {
                    Label("在 Emacs 中打开", systemImage: "arrow.up.forward.app")
                }
                .disabled(model.isSelectingSession || model.isSendingPrompt)
                .accessibilityIdentifier("open-session-in-emacs")
            }
        }
        .padding(.horizontal, 28).padding(.vertical, 20)
    }

    private func sessionOverview(_ session: AgentSession) -> some View {
        VStack(alignment: .leading, spacing: 18) {
            Image(systemName: "terminal")
                .font(.system(size: 26, weight: .medium))
                .foregroundStyle(.tint)
                .frame(width: 60, height: 60)
                .background(Color.accentColor.opacity(0.08), in: RoundedRectangle(cornerRadius: 17))
            Text("准备好下一步了吗？")
                .font(.system(size: 25, weight: .semibold))
            Text("向当前会话描述目标、补充上下文，或继续尚未完成的工作。")
                .font(.callout).foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 16) {
                HStack {
                    Label("会话状态", systemImage: "waveform.path")
                        .foregroundStyle(.secondary)
                    Spacer()
                    SessionStatusBadge(status: session.status)
                }
                Divider()
                VStack(alignment: .leading, spacing: 8) {
                    Label("项目目录", systemImage: "folder")
                        .foregroundStyle(.secondary)
                    Text(session.projectPath ?? "未提供项目目录")
                        .font(.system(.callout, design: .monospaced))
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .font(.callout).padding(18)
            .background(Color(nsColor: .windowBackgroundColor).opacity(0.7),
                        in: RoundedRectangle(cornerRadius: 14))

            Label("在 Emacs 中查看完整对话与执行结果", systemImage: "arrow.up.forward.app")
                .font(.caption).foregroundStyle(.secondary)
        }
        .padding(.top, 16)
    }

    private var composer: some View {
        @Bindable var model = model
        return VStack(alignment: .leading, spacing: 12) {
            if !model.sessionMessage.isEmpty && !model.sessions.isEmpty {
                Text(model.sessionMessage)
                    .font(.caption).foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("session-feedback")
            }
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 5) {
                    Image(systemName: "arrow.turn.down.right")
                    Text(selectedSession.map { "发送至 \($0.bufferName)" } ?? "先选择发送目标")
                        .lineLimit(1)
                }
                .font(.caption).foregroundStyle(.secondary)

                ZStack(alignment: .topLeading) {
                    if model.promptText.isEmpty {
                        Text("描述你的下一步需求…")
                            .foregroundStyle(.tertiary)
                            .padding(.leading, 5).padding(.top, 8)
                            .allowsHitTesting(false)
                    }
                    TextEditor(text: $model.promptText)
                        .font(.body)
                        .scrollContentBackground(.hidden)
                        .frame(minHeight: 80, maxHeight: 130)
                        .disabled(model.isSendingPrompt)
                        .accessibilityLabel("需求输入")
                }

                HStack {
                    Text(sendHint)
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    if model.isSendingPrompt { ProgressView().controlSize(.small) }
                    Button {
                        Task { await model.sendPrompt(configuration: configuration) }
                    } label: {
                        Label("发送需求", systemImage: "arrow.up")
                    }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.return, modifiers: .command)
                    .disabled(!canSend)
                }
            }
            .padding(16)
            .background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 16))
            .overlay {
                RoundedRectangle(cornerRadius: 16)
                    .strokeBorder(Color.primary.opacity(0.12))
            }
            Text("⌘ Return 发送 · Return 换行")
                .font(.caption2).foregroundStyle(.tertiary)
                .frame(maxWidth: .infinity)
        }
        .frame(maxWidth: 704)
        .padding(.horizontal, 28).padding(.bottom, 20).padding(.top, 8)
        .frame(maxWidth: .infinity)
    }

    private var canSend: Bool {
        selectedSession?.status == "ready" && !model.isSendingPrompt && !model.isSelectingSession
            && !model.promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private var sendHint: String {
        guard let session = selectedSession else { return "草稿会自动保留" }
        switch session.status {
        case "busy": return "会话运行中，刷新状态后再发送"
        case "ready": return "已就绪，可以发送需求"
        default: return "状态未知，请刷新后重试"
        }
    }

    private func projectName(_ session: AgentSession) -> String {
        session.projectPath.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "未关联项目"
    }
}

private struct SessionStatusBadge: View {
    let status: String

    var body: some View {
        HStack(spacing: 5) {
            Circle().frame(width: 5, height: 5)
            Text(title)
        }
        .font(.caption)
        .foregroundStyle(color)
        .padding(.horizontal, 7).padding(.vertical, 4)
        .background(color.opacity(0.08), in: Capsule())
    }

    private var title: String {
        switch status {
        case "ready": "就绪"
        case "busy": "运行中"
        default: "状态未知"
        }
    }

    private var color: Color {
        switch status {
        case "ready": .teal
        case "busy": .orange
        default: .secondary
        }
    }
}

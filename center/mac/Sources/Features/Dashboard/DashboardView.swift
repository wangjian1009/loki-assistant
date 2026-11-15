import AppKit
import SwiftUI

struct DashboardView: View {
    let model: ApplicationModel

    @AppStorage(EmacsConfiguration.executablePathKey)
    private var executablePath = EmacsConfiguration.defaultExecutablePath
    @AppStorage(EmacsConfiguration.socketNameKey)
    private var socketName = ""

    private var configuration: EmacsConfiguration {
        EmacsConfiguration(executablePath: executablePath, socketName: socketName)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack(spacing: 10) {
                Image(systemName: "terminal.fill")
                    .font(.title2)
                    .foregroundStyle(.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Loki Assistant").font(.headline)
                    Text("状态仪表盘").font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                if model.isCheckingConnection || model.isLoadingSessions {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("正在更新状态")
                }
            }

            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Circle().fill(connectionColor).frame(width: 7, height: 7)
                    Text(connectionTitle).font(.subheadline.weight(.medium))
                    Spacer()
                    Text(socketName.isEmpty ? "默认 server" : socketName)
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                Text(model.connectionMessage)
                    .font(.caption).foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(14)
            .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 12))

            HStack(spacing: 0) {
                metric("全部会话", count: model.sessions.count, color: .primary)
                Divider().frame(height: 32)
                metric("运行中", count: model.sessions.filter { $0.status == "busy" }.count, color: .orange)
                Divider().frame(height: 32)
                metric("就绪", count: model.sessions.filter { $0.status == "ready" }.count, color: .teal)
            }

            VStack(alignment: .leading, spacing: 5) {
                if let error = model.sessionRefreshError {
                    Label("会话状态更新失败", systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.orange)
                    Text(error).textSelection(.enabled)
                }
                if let date = model.lastSessionsRefresh {
                    HStack(spacing: 4) {
                        Text(model.sessionRefreshError == nil ? "最近更新" : "保留上次数据")
                        Text(date, style: .time)
                    }
                } else {
                    Text(model.isLoadingSessions ? "正在获取会话状态…" : "尚无会话状态数据")
                }
            }
            .font(.caption).foregroundStyle(.secondary)

            Divider()
            AgentShellWindowButton(model: model)
                .frame(maxWidth: .infinity)
            HStack {
                SettingsButton().fixedSize()
                Spacer()
                Button("退出") { NSApplication.shared.terminate(nil) }
                    .buttonStyle(.plain).foregroundStyle(.secondary)
            }
        }
        .padding(20)
        .frame(width: 340)
        .background(Color(nsColor: .windowBackgroundColor))
        .onChange(of: configuration, initial: true) { _, configuration in
            model.updateConfiguration(configuration)
        }
        .background {
            WindowVisibilityObserver {
                Task { await model.refreshStatus(configuration: configuration) }
            }
        }
    }

    private var connectionColor: Color {
        switch model.connectionState {
        case .unchecked: .secondary
        case .connected: .teal
        case .failed: .orange
        }
    }

    private var connectionTitle: String {
        if model.isCheckingConnection { return "正在检查连接" }
        switch model.connectionState {
        case .unchecked: return "等待连接"
        case .connected: return "Emacs 已连接"
        case .failed: return "Emacs 连接异常"
        }
    }

    private func metric(_ title: String, count: Int, color: Color) -> some View {
        VStack(spacing: 5) {
            Text(model.lastSessionsRefresh == nil ? "—" : "\(count)")
                .font(.title2.weight(.semibold)).monospacedDigit()
                .foregroundStyle(color)
            Text(title).font(.caption).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
    }
}

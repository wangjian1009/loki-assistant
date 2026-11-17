import AppKit
import SwiftUI

struct DashboardView: View {
    let status: ServiceStatusModel
    let reportedStatus: EmacsReportedStatusModel
    @State private var openingSettings = false
    @State private var settingsError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            statusButton
            Divider()
            monitoredBuffers
            Divider()
            Button {
                openingSettings = true
                Task { @MainActor in
                    defer { openingSettings = false }
                    do {
                        try await ServiceSettingsClient().open()
                    } catch {
                        settingsError = error.localizedDescription
                    }
                }
            } label: {
                Label(openingSettings ? "打开设置…" : "设置", systemImage: "slider.horizontal.3")
            }
            .disabled(openingSettings)
            .buttonStyle(.borderless)
            .frame(maxWidth: .infinity, alignment: .leading)
            Button {
                NSApplication.shared.terminate(nil)
            } label: {
                Label("退出", systemImage: "power")
            }
            .buttonStyle(.borderless)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(8)
        .frame(width: 300, alignment: .leading)
        .labelStyle(.titleAndIcon)
        .onAppear { reportedStatus.menuDidOpen() }
        .onDisappear { reportedStatus.menuDidClose() }
        .alert("无法打开配置", isPresented: Binding(
            get: { settingsError != nil },
            set: { if !$0 { settingsError = nil } }
        )) {
            Button("好") { settingsError = nil }
        } message: {
            Text(settingsError ?? "")
        }
        .alert("无法激活 buffer", isPresented: Binding(
            get: { reportedStatus.activationError != nil },
            set: { if !$0 { reportedStatus.activationError = nil } }
        )) {
            Button("好") { reportedStatus.activationError = nil }
        } message: {
            Text(reportedStatus.activationError ?? "")
        }
    }

    @ViewBuilder
    private var monitoredBuffers: some View {
        Text("关注的 buffer（\(reportedStatus.buffers.count)）")
            .font(.caption).foregroundStyle(.secondary)
        if let error = reportedStatus.refreshError {
            Text(error).font(.caption).foregroundStyle(.secondary)
        } else if reportedStatus.loading {
            Text("正在读取状态…").foregroundStyle(.secondary)
        } else if reportedStatus.hasSnapshot && reportedStatus.buffers.isEmpty {
            Text("没有关注的 buffer").foregroundStyle(.secondary)
        } else if !reportedStatus.hasSnapshot {
            Text("打开菜单后读取 Emacs 上报状态").foregroundStyle(.secondary)
        }
        ForEach(reportedStatus.buffers) { buffer in
            Button { reportedStatus.activate(buffer) } label: {
                Label(buffer.name, systemImage: "terminal")
                    .lineLimit(1)
            }
            .help("\(buffer.mode)\n\(buffer.directory)")
            .disabled(reportedStatus.activatingID != nil)
            .buttonStyle(.borderless)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder
    private var statusButton: some View {
        switch status.status {
        case let .healthy(health):
            Button {
            } label: {
                Label {
                    Text("\(health.version) · \(formatUptime(status.uptimeSeconds ?? health.uptimeSeconds))")
                        .monospacedDigit()
                } icon: {
                    statusDot
                }
            }
            .disabled(true)
            .buttonStyle(.borderless)
            .frame(width: 170, alignment: .leading)
        case .stopped:
            Button {
                Task { await status.start() }
            } label: {
                Label("启动", systemImage: "circle.fill")
                    .symbolRenderingMode(.palette)
                    .foregroundStyle(.gray, .gray)
            }
            .help("启动 services")
            .buttonStyle(.borderless)
            .frame(width: 170, alignment: .leading)
        case .unknown:
            statusInfoButton("检查中…")
        case .starting:
            statusInfoButton("启动中…")
        case .unavailable:
            statusInfoButton("不可用")
        }
    }

    private func statusInfoButton(_ title: String) -> some View {
        Button {
        } label: {
            Label {
                Text(title)
            } icon: {
                statusDot
            }
        }
        .disabled(true)
        .buttonStyle(.borderless)
        .frame(width: 170, alignment: .leading)
    }

    private var statusDot: some View {
        Image(systemName: "circle.fill")
            .font(.system(size: 9))
            .foregroundStyle(statusColor)
    }

    private var statusColor: Color {
        switch status.status {
        case .healthy:
            .green
        case .unknown:
            .gray
        case .starting:
            .gray
        case .stopped:
            .gray
        case .unavailable:
            .gray
        }
    }

    private func formatUptime(_ seconds: Int) -> String {
        let hours = seconds / 3600
        let minutes = (seconds % 3600) / 60
        let remainder = seconds % 60
        return String(format: "%02d:%02d:%02d", hours, minutes, remainder)
    }

}

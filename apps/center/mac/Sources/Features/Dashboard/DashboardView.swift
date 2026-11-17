import AppKit
import SwiftUI

struct DashboardView: View {
    let status: ServiceStatusModel
    @State private var openingSettings = false
    @State private var settingsError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            statusButton
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
        .frame(width: 130, alignment: .leading)
        .labelStyle(.titleAndIcon)
        .alert("无法打开配置", isPresented: Binding(
            get: { settingsError != nil },
            set: { if !$0 { settingsError = nil } }
        )) {
            Button("好") { settingsError = nil }
        } message: {
            Text(settingsError ?? "")
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

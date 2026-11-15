import AppKit
import SwiftUI

struct DashboardView: View {
    let status: ServiceStatusModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            statusButton
            Divider()
            Button {
                openConfigEditor()
            } label: {
                Label("设置", systemImage: "slider.horizontal.3")
            }
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
    }

    private func openConfigEditor() {
        let appURL = Bundle.main.bundleURL
            .deletingLastPathComponent() // Debug
            .deletingLastPathComponent() // .build
            .deletingLastPathComponent() // mac
            .deletingLastPathComponent() // center
            .deletingLastPathComponent() // apps
            .appendingPathComponent("config-editor/mac/.build/Debug/LokiConfigEditor.app")
        _ = NSWorkspace.shared.open(appURL)
    }

    @ViewBuilder
    private var statusButton: some View {
        switch status.status {
        case let .healthy(health):
            Button {
            } label: {
                Label {
                    Text("\(health.version) · \(formatUptime(health.uptimeSeconds))")
                } icon: {
                    statusDot
                }
            }
            .disabled(true)
            .buttonStyle(.borderless)
            .frame(maxWidth: .infinity, alignment: .leading)
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
            .frame(maxWidth: .infinity, alignment: .leading)
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
        .frame(maxWidth: .infinity, alignment: .leading)
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

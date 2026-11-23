import AppKit
import SwiftUI

struct DashboardView: View {
    let status: ServiceStatusModel
    let idleSleep: IdleSleepController
    let openAlert: @MainActor (String, String) -> Void
    let setDefaultMailApplication: @MainActor () -> Void
    @State private var openingSettings = false
    @State private var appOpenError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            statusSummary
            Divider()
            Toggle(isOn: Binding(
                get: { idleSleep.isEnabled },
                set: { enabled in
                    let operation = CenterLog.shared.start(
                        "切换阻止空闲休眠",
                        details: ["enabled": enabled ? "true" : "false"]
                    )
                    idleSleep.setEnabled(enabled)
                    CenterLog.shared.finish(
                        operation,
                        result: "success",
                        details: ["enabled": enabled ? "true" : "false"]
                    )
                }
            )) {
                Label(
                    idleSleep.isEnabled ? "已阻止空闲休眠" : "阻止空闲休眠",
                    systemImage: idleSleep.isEnabled ? "moon.zzz.fill" : "moon.zzz"
                )
            }
            Divider()
            if !status.activeAlerts.isEmpty {
                Section("告警") {
                    ForEach(status.activeAlerts) { alert in
                        if let callback = alert.callbackURL {
                            Button {
                                openAlert(callback, alert.id)
                            } label: {
                                Text(alert.menuTitle)
                            }
                        } else {
                            Text(alert.menuTitle)
                        }
                    }
                }
            }
            if !status.slackNewConversations.isEmpty {
                Section("Slack 未读会话") {
                    ForEach(status.slackNewConversations) { conversation in
                        Button {
                            openSlackConversation(conversation)
                        } label: {
                            if let count = conversation.unreadCount {
                                Text("\(conversation.title) · \(conversation.teamName) · \(count) 条未读")
                            } else {
                                Text("\(conversation.title) · \(conversation.teamName)")
                            }
                        }
                    }
                }
            }
            Button(action: setDefaultMailApplication) {
                Label("设为默认邮件应用", systemImage: "envelope")
            }
            .buttonStyle(.borderless)
            .frame(maxWidth: .infinity, alignment: .leading)
            Button {
                openingSettings = true
                Task { @MainActor in
                    defer { openingSettings = false }
                    let operation = CenterLog.shared.start("打开设置")
                    do {
                        try await ServiceSettingsClient().open(app: "config", operation: operation)
                        CenterLog.shared.finish(operation, result: "success")
                    } catch {
                        CenterLog.shared.finish(operation, result: "failure", details: ["error": error.localizedDescription])
                        appOpenError = error.localizedDescription
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
        .alert("无法打开设置", isPresented: Binding(
            get: { appOpenError != nil },
            set: { if !$0 { appOpenError = nil } }
        )) {
            Button("好") { appOpenError = nil }
        } message: {
            Text(appOpenError ?? "")
        }
    }

    private func openSlackConversation(_ conversation: SlackNewConversation) {
        Task { @MainActor in
            let operation = CenterLog.shared.start("打开 Slack 新消息")
            do {
                try await NotificationReceiptClient().openSlackConversation(conversation)
                CenterLog.shared.finish(operation, result: "success")
            } catch {
                CenterLog.shared.finish(operation, result: "failure", details: ["error": error.localizedDescription])
                let alert = NSAlert()
                alert.messageText = "无法打开 Slack 新消息"
                alert.informativeText = error.localizedDescription
                alert.alertStyle = .warning
                alert.addButton(withTitle: "好")
                NSApp.activate(ignoringOtherApps: true)
                alert.runModal()
            }
        }
    }

    @ViewBuilder
    private var statusSummary: some View {
        switch status.status {
        case let .healthy(version):
            Label {
                if let startedAt = status.startedAt {
                    Text("\(version) · \(networkStatusLabel) · 启动于 \(startedAt.formatted(.dateTime.year().month(.twoDigits).day(.twoDigits).hour(.twoDigits(amPM: .omitted)).minute(.twoDigits)))")
                        .monospacedDigit()
                } else {
                    Text("\(version) · \(networkStatusLabel)")
                }
            } icon: {
                statusDot
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        case .stopped:
            statusInfo("未运行")
        case .unknown:
            statusInfo("检查中…")
        case .unavailable:
            statusInfo("不可用")
        }
    }

    private func statusInfo(_ title: String) -> some View {
        Label {
            Text(title)
        } icon: {
            statusDot
        }
        .frame(width: 170, alignment: .leading)
    }

    private var statusDot: some View {
        Image(nsImage: MenuStatusDot.image(for: status.indicator))
            .renderingMode(.original)
    }

    private var networkStatusLabel: String {
        switch status.networkStatus {
        case "connected": "中继已连接"
        case "connecting": "中继连接中"
        case "failed": "中继连接失败"
        case "offline": "offline"
        case "disconnected": "中继未连接"
        default: "网络状态未知"
        }
    }

}

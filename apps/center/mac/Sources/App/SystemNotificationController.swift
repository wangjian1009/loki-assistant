import AppKit
import Foundation
import OSLog
import UserNotifications

struct EmacsSystemNotification: Codable, Equatable, Sendable {
    let title: String
    let icon: String?
    let buffer: String

    init(url: URL) throws {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.scheme == "loki-assistant",
              components.host == "emacs",
              components.path == "/notification",
              let payload = components.queryItems?.first(where: { $0.name == "payload" })?.value,
              let data = payload.data(using: .utf8) else {
            throw SystemNotificationError.invalidPayload
        }
        self = try JSONDecoder().decode(Self.self, from: data)
        guard !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !buffer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw SystemNotificationError.invalidPayload
        }
    }

    func makeContent() -> UNMutableNotificationContent {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = "点击激活 Emacs"
        content.sound = .default
        content.userInfo = ["kind": "emacs", "buffer": buffer]
        if let icon, !icon.isEmpty, !icon.hasPrefix("/"), !icon.hasPrefix("file:") {
            content.title = "\(icon) \(title)"
        }
        return content
    }
}

enum SystemNotificationError: LocalizedError {
    case invalidPayload
    case permissionDenied
    case emacsUnavailable

    var errorDescription: String? {
        switch self {
        case .invalidPayload: "系统通知数据无效"
        case .permissionDenied: "请在 macOS 系统设置中允许 Loki Assistant 发送通知"
        case .emacsUnavailable: "未找到 Emacs 应用"
        }
    }
}

@MainActor
final class CenterAppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    private let logger = Logger(subsystem: "cc.loki-assistant.center", category: "notification")

    func applicationDidFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls {
            Task {
                do {
                    try await present(EmacsSystemNotification(url: url))
                } catch {
                    logger.error("系统通知失败：\(error.localizedDescription, privacy: .public)")
                }
            }
        }
    }

    private func present(_ notification: EmacsSystemNotification) async throws {
        let center = UNUserNotificationCenter.current()
        let allowed = try await center.requestAuthorization(options: [.alert, .sound])
        guard allowed else { throw SystemNotificationError.permissionDenied }
        let content = notification.makeContent()
        let directory = FileManager.default.temporaryDirectory
            .appending(path: "LokiAssistantNotifications/\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        if let icon = notification.icon, icon.hasPrefix("/") || icon.hasPrefix("file:") {
            do {
                guard let source = icon.hasPrefix("file:") ? URL(string: icon) : URL(fileURLWithPath: icon),
                      source.isFileURL else { throw SystemNotificationError.invalidPayload }
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                let copy = directory.appending(path: source.lastPathComponent)
                try FileManager.default.copyItem(at: source, to: copy)
                content.attachments = [try UNNotificationAttachment(identifier: "icon", url: copy)]
            } catch {
                logger.error("通知图标无法显示：\(error.localizedDescription, privacy: .public)")
            }
        }
        try await center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        guard response.actionIdentifier == UNNotificationDefaultActionIdentifier,
              response.notification.request.content.userInfo["kind"] as? String == "emacs" else { return }
        await activateEmacs()
    }

    private func activateEmacs() async {
        let workspace = NSWorkspace.shared
        if let running = workspace.runningApplications.first(where: {
            $0.bundleIdentifier == "org.gnu.Emacs" || $0.localizedName == "Emacs"
        }) {
            running.activate(options: [.activateAllWindows])
            return
        }
        do {
            guard let url = workspace.urlForApplication(withBundleIdentifier: "org.gnu.Emacs") else {
                throw SystemNotificationError.emacsUnavailable
            }
            let configuration = NSWorkspace.OpenConfiguration()
            configuration.activates = true
            _ = try await workspace.openApplication(at: url, configuration: configuration)
        } catch {
            logger.error("无法激活 Emacs：\(error.localizedDescription, privacy: .public)")
        }
    }
}

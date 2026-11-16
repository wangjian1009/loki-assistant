import AppKit
import Foundation
import OSLog
import UserNotifications

struct SystemNotification: Codable, Equatable, Sendable {
    let notificationID: String?
    let title: String
    let body: String?
    let icon: String?
    let navigationURL: String?
    // 兼容已发出的旧版 Emacs 通知；新通知只携带通用跳转 URL。
    let buffer: String?

    enum CodingKeys: String, CodingKey {
        case notificationID = "notification_id"
        case navigationURL = "navigation_url"
        case title, body, icon, buffer
    }

    init(url: URL) throws {
        self = try JSONDecoder().decode(Self.self, from: Self.payload(url: url))
        _ = try Self.receiptID(url: url)
        guard !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              buffer.map({ !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }) ?? true else {
            throw SystemNotificationError.invalidPayload
        }
        if let navigationURL {
            guard let url = URL(string: navigationURL) else { throw SystemNotificationError.invalidPayload }
            _ = try NotificationReceiptClient.callbackPath(url)
        }
    }

    private static func payload(url: URL) throws -> Data {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.scheme == "loki-assistant",
              (components.host == "notification" && components.path == "/show") ||
              (components.host == "emacs" && components.path == "/notification"),
              let payload = components.queryItems?.first(where: { $0.name == "payload" })?.value,
              let data = payload.data(using: .utf8) else { throw SystemNotificationError.invalidPayload }
        return data
    }

    static func receiptID(url: URL) throws -> String? {
        struct Context: Decodable { let notification_id: String? }
        let id = try JSONDecoder().decode(Context.self, from: payload(url: url)).notification_id
        if let id, UUID(uuidString: id) == nil { throw SystemNotificationError.invalidPayload }
        return id
    }

    static func legacyNavigationURL(buffer: String?) -> URL {
        var callback = URLComponents()
        callback.path = "/emacs/activate"
        if let buffer { callback.queryItems = [URLQueryItem(name: "buffer", value: buffer)] }
        var navigation = URLComponents()
        navigation.scheme = "loki-assistant"
        navigation.host = "notification"
        navigation.path = "/activate"
        navigation.queryItems = [URLQueryItem(name: "callback", value: callback.string!)]
        return navigation.url!
    }

    func makeContent() -> UNMutableNotificationContent {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body ?? (buffer == nil ? "" : "点击激活 Emacs")
        content.sound = .default
        if let navigationURL { content.userInfo["navigation_url"] = navigationURL }
        if let buffer {
            content.userInfo["navigation_url"] = Self.legacyNavigationURL(buffer: buffer).absoluteString
            content.userInfo["kind"] = "emacs"
            content.userInfo["buffer"] = buffer
        }
        if let notificationID { content.userInfo["notification_id"] = notificationID }
        if let icon, !icon.isEmpty, !Self.isImage(icon) { content.title = "\(icon) \(title)" }
        return content
    }

    static func isImage(_ icon: String) -> Bool {
        icon.hasPrefix("/") || icon.hasPrefix("file:") || icon.hasPrefix("app-icon://")
    }
}

enum SystemNotificationError: LocalizedError {
    case invalidPayload
    case permissionDenied

    var errorDescription: String? {
        switch self {
        case .invalidPayload: "系统通知数据无效"
        case .permissionDenied: "请在 macOS 系统设置中允许 Loki Assistant 发送通知"
        }
    }
}

@MainActor
final class CenterAppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    private let logger = Logger(subsystem: "cc.loki-assistant.center", category: "notification")
    private let receiptClient = NotificationReceiptClient()

    func applicationWillFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls {
            Task {
                do {
                    if url.host == "notification", url.path == "/activate" {
                        try await receiptClient.activate(url)
                        logger.notice("应用内通知跳转已回调 services")
                        return
                    }
                    let delivery = NotificationDelivery(
                        present: { try await self.present($0) },
                        report: { try await self.receiptClient.send($1, notificationID: $0) }
                    )
                    try await delivery.handle(url)
                } catch {
                    logger.error("系统通知处理或回执失败：\(error.localizedDescription, privacy: .public)")
                }
            }
        }
    }

    private func present(_ notification: SystemNotification) async throws {
        let center = UNUserNotificationCenter.current()
        let allowed = try await center.requestAuthorization(options: [.alert, .sound])
        guard allowed else { throw SystemNotificationError.permissionDenied }
        let content = notification.makeContent()
        let directory = FileManager.default.temporaryDirectory
            .appending(path: "LokiAssistantNotifications/\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        if let icon = notification.icon, SystemNotification.isImage(icon) {
            do {
                let image: NSImage?
                if icon.hasPrefix("app-icon://") {
                    let bundleID = String(icon.dropFirst("app-icon://".count))
                    image = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
                        .map { NSWorkspace.shared.icon(forFile: $0.path) }
                } else {
                    let source = icon.hasPrefix("file:") ? URL(string: icon) : URL(fileURLWithPath: icon)
                    guard let source, source.isFileURL else { throw SystemNotificationError.invalidPayload }
                    image = NSImage(contentsOf: source)
                }
                guard let data = image?.tiffRepresentation,
                      let bitmap = NSBitmapImageRep(data: data),
                      let png = bitmap.representation(using: .png, properties: [:]) else {
                    throw SystemNotificationError.invalidPayload
                }
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                let copy = directory.appending(path: "icon.png")
                try png.write(to: copy)
                content.attachments = [try UNNotificationAttachment(identifier: "icon", url: copy)]
            } catch {
                logger.error("通知图标无法显示：\(error.localizedDescription, privacy: .public)")
            }
        }
        try await center.add(UNNotificationRequest(identifier: notification.notificationID ?? UUID().uuidString, content: content, trigger: nil))
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
        guard response.actionIdentifier == UNNotificationDefaultActionIdentifier else { return }
        let info = response.notification.request.content.userInfo
        let navigation: URL?
        if let value = info["navigation_url"] as? String {
            navigation = URL(string: value)
        } else if info["kind"] as? String == "emacs" {
            navigation = SystemNotification.legacyNavigationURL(buffer: info["buffer"] as? String)
        } else {
            return
        }
        guard let navigation else { return }
        await followNotificationNavigation(navigation, notificationID: response.notification.request.identifier)
    }

    private func followNotificationNavigation(_ navigation: URL, notificationID: String) async {
        do {
            try await receiptClient.activate(navigation)
            logger.notice("通知点击已回调 services：\(notificationID, privacy: .public)")
        } catch {
            logger.error("通知点击跳转失败：\(error.localizedDescription, privacy: .public)")
        }
    }
}

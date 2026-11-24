import AppKit
import Darwin
import Foundation
import UserNotifications

struct SystemNotification: Codable, Equatable, Sendable {
    let notificationID: String?
    let title: String
    let body: String?
    let app: String?
    let navigationURL: String?
    // 兼容已发出的旧版 Emacs 通知；新通知只携带通用跳转 URL。
    let buffer: String?

    enum CodingKeys: String, CodingKey {
        case notificationID = "notification_id"
        case navigationURL = "navigation_url"
        case title, body, app, buffer
    }

    init(url: URL) throws {
        self = try JSONDecoder().decode(Self.self, from: Self.payload(url: url))
        _ = try Self.receiptID(url: url)
        guard !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              buffer.map({ !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }) ?? true,
              app.map(Self.isValidApp) ?? true else {
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
        // services 的查询解析将 + 视为空格，名称中的字面 + 必须单独编码。
        callback.percentEncodedQuery = callback.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        return navigationURL(callback: callback.string!)
    }

    static func navigationURL(callback: String) -> URL {
        var navigation = URLComponents()
        navigation.scheme = "loki-assistant"
        navigation.host = "notification"
        navigation.path = "/activate"
        navigation.queryItems = [URLQueryItem(name: "callback", value: callback)]
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
        return content
    }

    private static func isValidApp(_ app: String) -> Bool {
        !app.isEmpty && app.utf8.allSatisfy {
            (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95
        }
    }

    func appIconURL(in bundle: Bundle = .main) -> URL? {
        guard let app, Self.isValidApp(app) else { return nil }
        return bundle.url(forResource: app, withExtension: "png", subdirectory: "NotificationAppIcons")
    }
}

enum SystemNotificationError: LocalizedError {
    case invalidPayload
    case permissionDenied

    var errorDescription: String? {
        switch self {
        case .invalidPayload: "系统通知数据无效"
        case .permissionDenied: "请在 macOS 系统设置中允许 Loki的助理 发送通知"
        }
    }
}

private final class CenterInstanceLock {
    private let descriptor: Int32

    init?() {
        let directory = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: ".assistant")
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let path = directory.appending(path: "center.lock").path
        let descriptor = open(path, O_CREAT | O_RDWR, 0o600)
        guard descriptor >= 0, flock(descriptor, LOCK_EX | LOCK_NB) == 0 else {
            if descriptor >= 0 { close(descriptor) }
            return nil
        }
        self.descriptor = descriptor
    }

    deinit {
        flock(descriptor, LOCK_UN)
        close(descriptor)
    }
}

@MainActor
final class CenterAppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    private let receiptClient = NotificationReceiptClient()
    private var instanceLock: CenterInstanceLock?
    private lazy var delivery = NotificationDelivery(
        present: { [unowned self] in try await self.present($0) },
        report: { [receiptClient] in try await receiptClient.send($1, notificationID: $0) }
    )

    func applicationWillFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
        // XCTest 的宿主进程使用独立标识，跳过正式实例锁和启动补发。
        let isTesting = ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
            || NSClassFromString("XCTestCase") != nil
        guard !isTesting else { return }

        instanceLock = CenterInstanceLock()
        if instanceLock == nil {
            let existing = NSRunningApplication.runningApplications(
                withBundleIdentifier: Bundle.main.bundleIdentifier ?? "cc.loki-assistant.center"
            ).first { $0.processIdentifier != getpid() }
            existing?.activate(options: [.activateIgnoringOtherApps, .activateAllWindows])
            DispatchQueue.main.async {
                NSApplication.shared.terminate(nil)
            }
        }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard instanceLock != nil else { return }
        CenterLog.shared.record("托盘已启动")
        Task {
            do {
                try await receiptClient.requestPendingRetry()
            } catch {
                CenterLog.shared.record("启动时请求补发通知失败：\(error.localizedDescription)", level: "WARN")
            }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        guard instanceLock != nil else { return }
        CenterLog.shared.record("托盘已退出")
        CenterLog.shared.flush()
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls {
            Task {
                if url.scheme?.lowercased() == "mailto" {
                    do {
                        try await receiptClient.composeMail(url)
                    } catch {
                        showError("无法打开邮件草稿", error: error)
                    }
                    return
                }
                do {
                    if url.host == "notification", url.path == "/activate" {
                        await followNotificationNavigation(url, notificationID: "URL")
                        return
                    }
                    try await delivery.handle(url)
                } catch {
                    CenterLog.shared.record("系统通知处理或回执失败：\(error.localizedDescription)", level: "ERROR")
                }
            }
        }
    }

    private func present(_ notification: SystemNotification) async throws {
        let center = UNUserNotificationCenter.current()
        if let id = notification.notificationID {
            let pending = await center.pendingNotificationRequests()
            let delivered = await center.deliveredNotifications()
            if pending.contains(where: { $0.identifier == id })
                || delivered.contains(where: { $0.request.identifier == id }) {
                return
            }
        }
        let allowed = try await center.requestAuthorization(options: [.alert, .sound])
        guard allowed else { throw SystemNotificationError.permissionDenied }
        let content = notification.makeContent()
        let directory = FileManager.default.temporaryDirectory
            .appending(path: "LokiAssistantNotifications/\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        if let source = notification.appIconURL() {
            do {
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                let copy = directory.appending(path: "app.png")
                try FileManager.default.copyItem(at: source, to: copy)
                content.attachments = [try UNNotificationAttachment(identifier: "app", url: copy)]
            } catch {
                CenterLog.shared.record("应用图标无法显示：\(error.localizedDescription)", level: "WARN")
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

    func openAlert(callback: String, id: String) {
        Task {
            await followNotificationNavigation(SystemNotification.navigationURL(callback: callback), notificationID: id)
        }
    }

    func openUnreadMail() {
        Task {
            let operation = CenterLog.shared.start("打开未读邮件列表")
            do {
                try await receiptClient.openUnreadMail()
                CenterLog.shared.finish(operation, result: "success")
            } catch {
                CenterLog.shared.finish(operation, result: "failure", details: ["error": error.localizedDescription])
                showError("无法打开未读邮件列表", error: error)
            }
        }
    }

    private func followNotificationNavigation(_ navigation: URL, notificationID: String) async {
        do {
            try await receiptClient.activate(navigation)
            CenterLog.shared.record("通知点击已回调 services：\(notificationID)")
        } catch {
            CenterLog.shared.record("通知点击跳转失败：\(error.localizedDescription)", level: "ERROR")
            showError("无法完成通知跳转", error: error)
        }
    }

    private func showError(_ title: String, error: Error) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = error.localizedDescription
        alert.alertStyle = .warning
        alert.addButton(withTitle: "好")
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }
}

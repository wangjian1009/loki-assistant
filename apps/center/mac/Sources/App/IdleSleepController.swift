import Foundation
import Observation

/// 管理菜单栏应用的“阻止空闲休眠”开关。
///
/// 该 activity 只对应 `PreventUserIdleSystemSleep`，不会阻止用户主动睡眠、合盖或锁屏。
@MainActor
@Observable
final class IdleSleepController {
    static let userDefaultsKey = "preventIdleSystemSleep"

    private(set) var isEnabled: Bool

    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var activity: NSObjectProtocol?

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        self.isEnabled = defaults.bool(forKey: Self.userDefaultsKey)
        if isEnabled {
            activity = ProcessInfo.processInfo.beginActivity(
                options: [.idleSystemSleepDisabled],
                reason: "Loki的助理：用户已开启阻止空闲休眠"
            )
        }
    }

    func setEnabled(_ enabled: Bool) {
        guard enabled != isEnabled else { return }
        if enabled {
            activity = ProcessInfo.processInfo.beginActivity(
                options: [.idleSystemSleepDisabled],
                reason: "Loki的助理：用户已开启阻止空闲休眠"
            )
        } else if let activity {
            ProcessInfo.processInfo.endActivity(activity)
            self.activity = nil
        }
        isEnabled = enabled
        defaults.set(enabled, forKey: Self.userDefaultsKey)
    }

    func toggle() {
        setEnabled(!isEnabled)
    }
}

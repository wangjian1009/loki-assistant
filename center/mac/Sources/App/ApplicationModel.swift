import Foundation
import Observation

@MainActor
@Observable
final class ApplicationModel {
    private(set) var connectionMessage = "尚未连接 Emacs"
    private(set) var isCheckingConnection = false

    func checkConnection(configuration: EmacsConfiguration) async {
        guard !isCheckingConnection else { return }
        isCheckingConnection = true
        connectionMessage = "正在连接…"
        defer { isCheckingConnection = false }

        do {
            let info = try await EmacsClient().ping(configuration: configuration)
            connectionMessage = "已连接 Emacs \(info.emacsVersion)"
        } catch {
            connectionMessage = error.localizedDescription
        }
    }
}

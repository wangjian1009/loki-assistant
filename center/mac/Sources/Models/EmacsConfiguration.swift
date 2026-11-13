import Foundation

struct EmacsConfiguration: Sendable {
    static let executablePathKey = "emacsclientPath"
    static let socketNameKey = "emacsSocketName"

    static var defaultExecutablePath: String {
        let candidates = [
            "/opt/homebrew/bin/emacsclient",
            "/usr/local/bin/emacsclient",
            "/usr/bin/emacsclient"
        ]
        return candidates.first {
            FileManager.default.isExecutableFile(atPath: $0)
        } ?? candidates[0]
    }

    let executablePath: String
    let socketName: String
}

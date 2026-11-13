import SwiftUI

struct SettingsView: View {
    @AppStorage(EmacsConfiguration.executablePathKey)
    private var executablePath = EmacsConfiguration.defaultExecutablePath
    @AppStorage(EmacsConfiguration.socketNameKey)
    private var socketName = ""

    var body: some View {
        Form {
            TextField("emacsclient 路径", text: $executablePath)
            TextField("Server 名称或 socket 路径", text: $socketName)
            Text("Server 留空时连接默认 Emacs server。")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .textFieldStyle(.roundedBorder)
        .padding(24)
        .frame(width: 540)
    }
}

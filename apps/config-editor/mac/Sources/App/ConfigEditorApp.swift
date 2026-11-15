import SwiftUI

@main
struct ConfigEditorApp: App {
    @State private var model = ConfigEditorModel()

    var body: some Scene {
        WindowGroup("配置编辑器") {
            ConfigEditorView(model: model)
        }
    }
}

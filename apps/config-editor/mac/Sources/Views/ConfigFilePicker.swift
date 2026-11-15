import AppKit

@MainActor
final class ConfigFilePicker: NSObject {
    private let item: ConfigItemMetadata

    init(item: ConfigItemMetadata) {
        self.item = item
    }

    func begin(currentValue: String, completion: @escaping (URL?) -> Void) {
        let panel = NSOpenPanel()
        let directory = item.dataType == .filePath && item.filePath?.directory == true
        panel.title = "选择 \(item.name)"
        panel.message = item.description
        panel.prompt = "选择"
        panel.allowsMultipleSelection = false
        panel.canChooseFiles = !directory
        panel.canChooseDirectories = directory
        panel.canCreateDirectories = directory && item.filePath?.mustExist == false
        panel.treatsFilePackagesAsDirectories = item.dataType == .executable
        if currentValue.hasPrefix("/") {
            let current = URL(fileURLWithPath: currentValue)
            panel.directoryURL = directory ? current : current.deletingLastPathComponent()
            if !directory { panel.nameFieldStringValue = current.lastPathComponent }
        } else if let suggested = item.executable?.searchPaths.first(where: {
            var directory: ObjCBool = false
            return FileManager.default.fileExists(atPath: $0, isDirectory: &directory) && directory.boolValue
        }) {
            panel.directoryURL = URL(fileURLWithPath: suggested)
        }
        panel.begin { response in
            self.complete(panel: panel, response: response, completion: completion)
        }
    }

    private func complete(panel: NSOpenPanel, response: NSApplication.ModalResponse, completion: (URL?) -> Void) {
        completion(response == .OK ? panel.url : nil)
    }

}

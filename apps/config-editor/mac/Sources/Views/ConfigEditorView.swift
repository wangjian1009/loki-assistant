import SwiftUI

struct ConfigEditorView: View {
    let model: ConfigEditorModel

    var body: some View {
        NavigationSplitView {
            List(model.modules, selection: Binding(
                get: { model.selectedModuleID },
                set: { value in
                    guard let value else { return }
                    Task { await model.select(moduleID: value) }
                }
            )) { module in
                VStack(alignment: .leading) {
                    Text(module.name)
                    Text(module.id)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .tag(module.id)
            }
            .navigationTitle("配置模块")
        } detail: {
            detail
        }
        .frame(minWidth: 760, minHeight: 480)
    }

    @ViewBuilder
    private var detail: some View {
        if let module = model.modules.first(where: { $0.id == model.selectedModuleID }) {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(module.name).font(.title2)
                    Text(module.description).foregroundStyle(.secondary)
                }
                if module.items.isEmpty {
                    ContentUnavailableView("没有配置项", systemImage: "slider.horizontal.3", description: Text("该模块暂未提供配置模型"))
                } else {
                    List(module.items) { item in
                        ConfigItemRow(item: item, model: model)
                    }
                    .listStyle(.inset)
                }
                if let message = model.message {
                    Text(message).font(.caption).foregroundStyle(.secondary)
                }
            }
            .padding()
        } else if model.loading {
            ProgressView("读取配置模型…")
        } else {
            ContentUnavailableView("没有配置模块", systemImage: "slider.horizontal.3")
        }
    }
}

private struct ConfigItemRow: View {
    let item: ConfigItemMetadata
    let model: ConfigEditorModel
    @State private var draft = ""

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(item.name)
                Text(item.key).font(.caption).foregroundStyle(.secondary)
                if !item.description.isEmpty {
                    Text(item.description).font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
            TextField("值", text: $draft)
                .textFieldStyle(.roundedBorder)
                .frame(width: 260)
            Button("保存") {
                Task { await model.save(item, text: draft) }
            }
        }
        .task { draft = model.draft(for: item) }
    }
}

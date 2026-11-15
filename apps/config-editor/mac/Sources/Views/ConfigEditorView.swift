import SwiftUI

struct ConfigEditorView: View {
    let model: ConfigEditorModel

    var body: some View {
        Group {
            if model.serviceAvailable == false {
                ContentUnavailableView("服务没有运行", systemImage: "power")
            } else if model.serviceAvailable == nil {
                ProgressView("检查服务状态…")
            } else if model.modules.isEmpty {
                ContentUnavailableView("没有可编辑的配置", systemImage: "slider.horizontal.3")
            } else {
                editor
            }
        }
        .task {
            await model.load()
        }
    }

    private var editor: some View {
        NavigationSplitView {
            VStack(spacing: 8) {
                TextField("搜索", text: $searchText)
                    .textFieldStyle(.roundedBorder)
                    .padding(.horizontal, 10)
                List(filteredModules, selection: Binding(
                    get: { model.selectedModuleID },
                    set: { value in
                        guard let value else { return }
                        Task { await model.select(moduleID: value) }
                    }
                )) { module in
                    Label(module.name, systemImage: "slider.horizontal.3")
                        .tag(module.id)
                }
            }
            .navigationTitle("配置模块")
        } detail: {
            detail
        }
        .toolbar {
            ToolbarItemGroup(placement: .navigation) {
                Button {
                    Task { await model.goBack() }
                } label: {
                    Image(systemName: "chevron.left")
                }
                .disabled(!model.canGoBack)
                Button {
                    Task { await model.goForward() }
                } label: {
                    Image(systemName: "chevron.right")
                }
                .disabled(!model.canGoForward)
            }
        }
        .frame(minWidth: 760, minHeight: 480)
    }

    @State private var searchText = ""

    private var filteredModules: [ConfigModuleMetadata] {
        guard !searchText.isEmpty else { return model.modules }
        return model.modules.filter {
            $0.name.localizedCaseInsensitiveContains(searchText) ||
            $0.id.localizedCaseInsensitiveContains(searchText)
        }
    }

    @ViewBuilder
    private var detail: some View {
        if let module = model.modules.first(where: { $0.id == model.selectedModuleID }) {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(module.name)
                        .font(.largeTitle)
                        .fontWeight(.semibold)
                    Text(module.description)
                        .font(.body)
                        .foregroundStyle(.secondary)
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
                Text(item.name).font(.headline)
                Text(item.key).font(.caption).foregroundStyle(.secondary)
                if !item.description.isEmpty {
                    Text(item.description).font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
            if item.dataType == .boolean {
                Toggle("", isOn: Binding(
                    get: { draft == "true" },
                    set: { value in
                        draft = String(value)
                        Task { await model.save(item, text: draft) }
                    }
                ))
                .labelsHidden()
                .toggleStyle(.switch)
            } else {
                TextField("值", text: $draft)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 260)
                    .onSubmit {
                        Task { await model.save(item, text: draft) }
                    }
            }
        }
        .task { draft = model.draft(for: item) }
    }
}

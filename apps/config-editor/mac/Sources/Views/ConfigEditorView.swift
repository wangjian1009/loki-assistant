import SwiftUI

struct ConfigEditorView: View {
    let model: ConfigEditorModel

    var body: some View {
        Group {
            if model.serviceAvailable == false {
                ContentUnavailableView {
                    Label("无法读取配置", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(model.message ?? "请确认 services 正在运行")
                } actions: {
                    Button("重试") { Task { await model.load() } }
                        .disabled(model.loading)
                }
            } else if model.serviceAvailable == nil {
                ProgressView("检查服务状态…")
            } else if model.modules.isEmpty {
                ContentUnavailableView("没有可编辑的配置", systemImage: "slider.horizontal.3")
            } else {
                editor
                    .disabled(model.loading)
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
                Button {
                    Task { await model.load() }
                } label: {
                    Label("刷新", systemImage: "arrow.clockwise")
                }
                .disabled(model.loading)
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
                }
                if module.items.isEmpty {
                    ContentUnavailableView("没有配置项", systemImage: "slider.horizontal.3", description: Text("该模块暂未提供配置模型"))
                } else {
                    List(module.items) { item in
                        ConfigItemRow(item: item, moduleID: module.id, model: model)
                    }
                    .listStyle(.inset)
                }
                if let message = model.message {
                    Text(message).font(.caption)
                        .foregroundStyle(model.messageIsError ? Color.red : Color.secondary)
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
    let moduleID: String
    let model: ConfigEditorModel
    @State private var draft = ""
    @State private var validationMessage: String?

    private var isDefaultValue: Bool {
        model.source(for: item) == .default && draft == model.draft(for: item)
    }

    private var valueColor: Color {
        if errorMessage != nil { return .red }
        return isDefaultValue ? .secondary : .primary
    }

    private var errorMessage: String? {
        validationMessage ?? model.saveError(for: item, text: draft, moduleID: moduleID)
    }

    var body: some View {
        HStack(spacing: 12) {
            Text(item.name)
                .lineLimit(1)
                .layoutPriority(1)
            Spacer(minLength: 12)
            if item.dataType == .boolean {
                Toggle("", isOn: Binding(
                    get: { draft == "true" },
                    set: { value in
                        draft = String(value)
                        Task { await model.save(item, text: draft, moduleID: moduleID) }
                    }
                ))
                .labelsHidden()
                .toggleStyle(.switch)
                .tint(isDefaultValue ? Color.secondary : Color.accentColor)
                .opacity(isDefaultValue ? 0.65 : 1)
                .overlay {
                    if errorMessage != nil {
                        RoundedRectangle(cornerRadius: 6).stroke(.red, lineWidth: 1)
                    }
                }
                .help(errorMessage ?? (isDefaultValue ? "默认值" : ""))
            } else {
                HStack(spacing: 8) {
                    TextField("", text: $draft)
                        .textFieldStyle(.roundedBorder)
                        .foregroundStyle(valueColor)
                        .font(isDefaultValue ? .body.italic() : .body)
                        .frame(minWidth: 140, idealWidth: 260, maxWidth: 360)
                        .overlay {
                            if errorMessage != nil {
                                RoundedRectangle(cornerRadius: 5).stroke(.red, lineWidth: 1)
                            }
                        }
                        .help(errorMessage ?? (isDefaultValue ? "默认值" : ""))
                        .onSubmit {
                            Task { await model.save(item, text: draft, moduleID: moduleID) }
                        }
                    if item.dataType == .executable || item.dataType == .filePath {
                        Button("选择…") {
                            ConfigFilePicker(item: item).begin(currentValue: draft) { url in
                                guard let url else { return }
                                draft = url.path
                                Task { await model.save(item, text: draft, moduleID: moduleID) }
                            }
                        }
                        .fixedSize()
                        .help(item.dataType == .executable ? "选择可执行文件" : "选择路径")
                    }
                }
            }
        }
        .padding(.vertical, 4)
        .task(id: model.draft(for: item)) { draft = model.draft(for: item) }
        .task(id: [moduleID, item.key, draft, String(model.loadRevision)]) {
            validationMessage = nil
            do { try await Task.sleep(for: .milliseconds(250)) }
            catch { return }
            let error = await model.validationError(for: item, text: draft, moduleID: moduleID)
            guard !Task.isCancelled else { return }
            validationMessage = error
        }
    }
}

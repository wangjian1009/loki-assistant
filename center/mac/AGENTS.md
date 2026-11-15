## 职责与技术路线

本目录实现 macOS 菜单栏应用，继承根目录和 `center/AGENTS.md` 的约定。

## 技术选型

- 工程：使用 CMake 3.30 及以上管理，以 Xcode generator 生成标准 macOS App 工程；应用名称 `LokiAssistant`，Swift 6 语言模式，最低支持 macOS 14。
- 应用生命周期：SwiftUI `App`，以 `MenuBarExtra` 提供菜单栏入口，使用 `.menuBarExtraStyle(.window)` 承载只读状态仪表盘。
- 界面：SwiftUI 原生控件，独立会话窗口使用 `HSplitView`、`List` 和 `TextEditor`，原生 `Settings` scene 承载配置界面。
- 菜单栏图标：SF Symbols 模板图标，适配系统明暗外观。
- 状态：Observation 的 `@Observable` 配合 `@MainActor` 管理界面状态；第一阶段不引入第三方状态管理框架。
- 通信：Foundation `Process` / `Pipe` 调用 `emacsclient`，Swift Concurrency 管理异步任务；JSON 使用 `Codable`，Base64 使用 `Data`。
- 系统集成：通过 AppKit 的 `NSWorkspace` / `NSRunningApplication` 定位并激活 Emacs。
- 配置：`UserDefaults`，界面使用 `@AppStorage` 绑定简单配置；第一阶段不引入数据库。

`MenuBarExtra` 只展示连接状态、会话统计和更新时间，底部提供会话窗口、设置与退出入口；不承载会话列表、切换、输入或手动连接检查。不同时维护另一套 `NSStatusItem` / `NSPopover` 入口。

会话管理由 `AgentShellWindowPresenter` 按需创建和复用一个普通 `NSWindow`，内容使用 SwiftUI。打开时收起来源菜单栏面板，待点击处理完成后激活并置前窗口；支持缩放、最小化恢复及关闭后重开，并记住窗口位置和大小。浏览列表只更改本地发送目标，显式点击“在 Emacs 中打开”才调用切换协议并激活 Emacs。

使用 `LSUIElement = YES` 作为菜单栏常驻应用运行，启动时不创建主窗口，不显示 Dock 图标。按需提供独立会话窗口，以及设置和退出入口。

设置入口使用原生 `Settings` scene；从实际点击的 AppKit 按钮获取来源窗口并收起菜单栏面板，跟踪 scene 创建的窗口，在点击处理完成后激活应用并将设置窗口置前，支持后台窗口再次置前和关闭后重开。

第一阶段用于本机个人使用，App Sandbox 关闭，以支持启动外部 `emacsclient` 并访问用户的 Emacs server。控制操作经插件完成，不通过辅助功能权限模拟输入。

## 目录结构

已建立应用与 XCTest target，由 CMake 生成 `LokiAssistant` scheme。构建配置以 `CMakeLists.txt` 为准，增加源码时须加入对应 target 的源文件列表。禁止直接修改生成的 Xcode 工程或 scheme。

```text
CMakeLists.txt            应用、XCTest target 与 Xcode 生成配置
cmake/
  Info.plist.in           菜单栏应用的 bundle 配置模板
Sources/
  App/                    SwiftUI App、窗口可见性观察与应用状态
  Features/
    AgentShell/           独立窗口管理、会话列表、筛选与需求输入
    Dashboard/            菜单栏只读状态仪表盘
    Settings/             emacsclient 路径、server 等配置界面
  Emacs/                  子进程调用、协议编码、连接配置与错误处理
  Models/                 会话、请求、响应及状态模型
Tests/                    XCTest 单元测试，覆盖通信与关键状态逻辑
.build/                   生成的 Xcode 工程、构建与测试产物，不提交
```

当前使用 SF Symbols，不创建空资源目录。实际需要图标资产时，再建立 `Resources/Assets.xcassets`。

## 构建约定

- 使用 out-of-source 构建，默认生成到 `.build/`；不在源码目录生成 `.xcodeproj` 或缓存文件。
- 常用操作通过根目录 Makefile 执行；它调用 CMake 构建、CTest 测试，CTest 再调用生成的 Xcode scheme 运行 app-hosted XCTest。
- 默认启用 `BUILD_TESTING`。测试构建启用 testability，并关闭 hardened runtime，以便本机临时签名的应用加载测试 bundle。
- 仅构建应用时可在独立构建目录设置 `-DBUILD_TESTING=OFF`，该配置保留 hardened runtime。

## 当前实现范围

- 已实现菜单栏状态仪表盘、独立会话窗口、设置与退出入口，以及基于 `ping` 的连接检查。
- 已实现协议编解码、请求标识与版本校验、进程超时和错误提示。
- 已接入真实 agent-shell 会话列表、名称与项目路径搜索、状态筛选、显式在 Emacs 中打开及需求发送。仪表盘和会话窗口每次显示时自动更新连接与列表，保留应用级选中状态和草稿。
- 构建和测试命令见 `../README.org`。

## 实现约定

- 第一阶段使用 Apple 原生框架，不引入菜单栏、窗口或进程管理的第三方依赖。
- 界面操作和状态更新运行在主 actor；通信实现须通过 Swift 6 并发检查，不为通过编译而禁用检查。
- 关闭仪表盘或会话窗口时保留选定会话和未发送的输入；状态由应用级模型持有，不依赖视图持续存活。
- 界面通过 Emacs 通信模块访问插件，不自行构造散落的 Elisp 表达式。
- 使用异步 `Process` 调用可执行文件，分别传递参数，不经 shell 拼接用户输入。
- 支持配置 `emacsclient` 路径与 server，禁止依赖交互式 shell 的 PATH。
- 调用设置超时，分别处理退出码、标准错误及协议错误；连接失败不得自动启动另一套 Emacs。
- 请求与响应按 `../../docs/agent-shell-control.org` 编解码。返回值不等于 agent 已完成任务。
- 仪表盘和会话窗口通过所属窗口的可见性变化更新状态，会话窗口另提供手动刷新，不引入推送服务；窗口内容被系统保留时也必须在重新展示后刷新。
- 切换到会话成功后，使用插件返回的 `emacs_pid` 激活目标 Emacs 进程，不按应用名选择实例；Emacs 内显示目标 buffer 由插件完成。
- 刷新成功或操作确认会话失效后清理对应选中状态，保留草稿；暂时连接失败时允许保留原列表以便重试。
- 修改连接设置后清理旧列表和选中项，保留草稿；旧请求返回时不得影响新连接的状态或激活旧 Emacs，切回原设置也不能重新接纳旧响应。
- 不在 UI 线程执行阻塞进程等待；优先验证连接失败、超时、编码与会话失效等关键路径。

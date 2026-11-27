## 职责

`apps/harmony/` 承载 Loki Assistant 的 HarmonyOS 手机执行中心。应用通过 Stage 模式 ArkTS 提供手机端入口，通过 iroh 与电脑完成扫码绑定和状态查询；业务 HTTP 服务在电脑本机提供。

## 约定

- `entry/src/main/ets/entryability/EntryAbility.ets` 是应用入口 Ability。
- `entry/src/main/ets/pages/Index.ets` 提供当前设备页的导航和生命周期。`components/CurrentDeviceWidgets.ets` 统一排列设备信息、邮件和 Slack 控件，负责界面拖动与 Preferences 保存。`app/CurrentDeviceLayout.ets` 定义控件 ID 和默认顺序，`app/CurrentDeviceState.ets` 管理当前设备状态监听。`pages/Devices.ets` 负责用 Scan Kit 扫码添加电脑、选择唯一 Network 目标和处理绑定失败记录。`pages/Settings.ets` 负责手机名称编辑。三个页面共用 `components/AppPage.ets` 的左侧导航。`ets/app/Application.ets` 管理电脑列表、手机名称、目标选择、绑定结果与完整配置持久化。
- `ets/app/NavigatorManager.ets` 统一管理首屏、应用内导航和完整的逻辑页面路径。EntryAbility 在加载主窗口前解析 Want，首屏只加载路径的末级页面，逐级返回时再加载尚未显示的父页面。Want 和 Debug 导航测试接口使用 `Index`、`Devices`、`Settings` 页面名称，由 NavigatorManager 转换为 ArkUI 页面路径。测试服务通过 `httpApiEnabled` 临时启停，仅监听设备回环地址。
- `components/PopupMenu.ets` 提供可复用的弹出菜单。调用方传入触发内容和带稳定 ID 的菜单项，组件按菜单项文字测量宽度并统一外观。
- Rust `Workspace` 创建 Tokio runtime 与唯一 `Arc<Network>`，不从应用数据中自动选择目标。调试 HTTP 监听与连接由同一个 Workspace 持有，导航请求经 NAPI 交给 ArkTS，`GET /service/…` 复用当前 Network 与绑定凭据。`storage.rs` 只解释本机随机身份，其余 JSON 由应用层解释，沿用私有 `devices.json`、`0600` 权限与原子替换；损坏禁止覆盖，私钥不向 ArkTS 导出。
- `Network` 只设置一个 `Service`，提供 `set_service()`、`service()`、异步 `request()` 和 `request_sse()`，不提供公开 `bind()` 或确认方法。未绑定配置中的 `auto_binding` 交给 Network 自动异步执行，成功保存凭据、失败保存错误，结果通过 `service()` 获取。业务保存返回的完整 `endpoint_id/relay_url/relay_token/binding`，下次读取后重新设置恢复连接。不得按请求重建 Workspace、查找全局 Network 或维护多个 Network 目标。
- `BindingState` 使用 `Unbound` / `Binding` / `Failed { error }` / `Bound { credential }` 数据 Union，凭据由电脑绑定接口签发并用于每次业务请求，不能以业务布尔值或绑定时间代替。只有绑定接口允许不携带凭据；状态查询也必须验证身份和 token。
- `ResponsError` 是直接返回的数据 Union，分为未设置、未绑定、网络、取消、Services 和参数错误，各分支分别定义 reason。ArkTS 请求采用成功/错误回调，直接返回 Union，不包装成 Error 类。
- `library/service-proxy` 提供网络绑定、请求与响应类型、缓存以及泛型监听。邮件和 Slack 各自定义服务端交互与业务类型；HarmonyOS 的界面组件分别位于 `ets/features/mail/`、`ets/features/slack/`。应用内 `workspace.rs` 负责实例和生命周期，`rust/src/napi.rs` 用 Rust 宏导出 NAPI 绑定。完整配置供业务存储，凭据不进入 UI、错误、监控或日志，Rust Debug 隐藏凭据。
- NAPI 只向 UI 暴露 Rust 已声明的类型和方法，由 `apps/harmony/` 内的 Rust 宏生成绑定及 ArkTS 类型声明。每个导出的类型和方法与 Rust 声明一一对应，保留所属类型、参数、返回值、错误及回调语义。
- NAPI 仅负责跨语言值转换、Rust 对象句柄与回调转发。禁止在绑定中新增业务字段、合并或拆分 Rust 方法、以字符串操作名代替显式方法，或加入业务校验、状态管理和流程编排。业务规则、状态结构和生命周期由对应的 Rust 模块定义。
- `Workspace.stop()` 先同步通知网络关闭，再等待未完成请求取消回调并释放 runtime；NAPI 取消和停止信号不等待异步工作线程。停止须幂等，停止后不隐式重新打开。
- 手机绑定时上报在设置页编辑的本机显示名称和 `device_type: mobile`。服务代理订阅 `GET /status` 的 SSE 流，同步电脑名称、设备类型、Slack 和邮件状态。邮件业务通过代理发起 `POST /mail/sync`，优先从邮件 SSE 接收进度，必要时由共享状态订阅跟踪任务。名称和类型仅用于展示。
- 界面只显示离线、绑定中、绑定失败、在线。界面注册服务代理监听，退到后台时解除。代理监听绑定结果，且有本地观察者时为当前选中电脑维持一条状态流。relay 断开、电脑连接关闭或状态流中断后发布离线状态，relay 可用时重新订阅。失败错误来自 Service 的 ResponsError，提示删除后重新扫码绑定；删除当前记录清除 Network，保存失败保留原记录。不存在待确认状态或旧邀请重试恢复；自动绑定结束后清除一次性 `auto_binding`，失败结果持久化后不自动重试。

- `scripts/build-device-network.sh` 使用 Rust `aarch64-unknown-linux-ohos` 与 DevEco NDK 构建 ARM64 `liblokiworkspace.so`。`make test` 测试真实应用层代码与 Rust 网络模块；services 的真实 relay 测试调用手机接口，验证凭据和 HTTP 分发。
- `bundleName` 为 `cc.github.wangjian1009.assistant`，由 `AppScope/app.json5` 定义。
- 应用和 Ability 引用 `$media:app_layered_icon`，由系统裁切桌面图标的底板。启动窗口引用静态 `$media:app_icon`。图标 PNG 由根目录 `make export-icons` 从 `resources/icons/` 公共母版导出，不手工编辑。
- 使用 DevEco Studio 内置 Hvigor 构建；常用操作统一维护在本目录 `Makefile` 和根目录 `Makefile` 中。
- 构建产物、Hvigor 缓存和依赖目录不提交到仓库。
- `signing/` 管理调试证书、Profile、密码保护的 PKCS#12 密钥库和签名配置。密钥库密码从 macOS Keychain 读取。签名工具的编译产物放在 Git 忽略的 `.signing/`。
- `make build` 先检查 SDK 并安装 OHPM 依赖，显式指定 Debug 模式与可调试标记，通过 Hvigor 的 `PackingCheck` 和 `CollectDebugSymbol` 打包，再由 `scripts/sign-hap.cjs` 独立签名；安装入口只使用已签名 HAP。
- `install`、`run`、`deploy` 统一通过 `scripts/select-hdc-target.sh` 选择设备；默认选择唯一已连接真机。`install` 和 `run` 可显式指定模拟器，`deploy` 始终要求真机并顺序完成构建、安装、启动。

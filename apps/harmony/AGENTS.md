## 职责

`apps/harmony/` 承载 Loki Assistant 的 HarmonyOS 手机执行中心。应用通过 Stage 模式 ArkTS 提供手机端入口，通过 iroh 与电脑完成扫码绑定和状态查询；业务 HTTP 服务在电脑本机提供。

## 约定

- `entry/src/main/ets/entryability/EntryAbility.ets` 是应用入口 Ability。
- `entry/src/main/ets/pages/Index.ets` 是深色设备入口，使用 Scan Kit 扫码；多设备使用 Swiper 单页展示，左右滑动同步选择唯一 Network 目标，设备概览固定、监控区域独立滚动；`pages/Settings.ets` 是设置页，负责手机名称编辑，首页仅保留设置入口；`ets/app/Application.ets` 在应用层管理电脑列表、手机名称、目标选择、绑定结果与完整配置持久化，不保留独立 devices 管理模块。
- `ets/app/NavigatorManager.ets` 统一管理首屏、应用内导航和完整的逻辑页面路径。EntryAbility 在加载主窗口前解析 Want，首屏只加载路径的末级页面，逐级返回时再加载尚未显示的父页面。Want 和 Debug 导航测试接口使用 `Index`、`Settings` 等页面名称，由 NavigatorManager 转换为 ArkUI 页面路径。测试服务通过 `httpApiEnabled` 临时启停，仅监听设备回环地址。
- Rust `Workspace` 创建 Tokio runtime 与唯一 `Arc<Network>`，不从应用数据中自动选择目标。调试 HTTP 监听与连接由同一个 Workspace 持有，导航请求经 NAPI 交给 ArkTS，`GET /service/…` 复用当前 Network 与绑定凭据。`storage.rs` 只解释本机随机身份，其余 JSON 由应用层解释，沿用私有 `devices.json`、`0600` 权限与原子替换；损坏禁止覆盖，私钥不向 ArkTS 导出。
- `Network` 只设置一个 `Service`，提供 `set_service()`、`service()` 和异步 `request()`，不提供公开 `bind()` 或确认方法。未绑定配置中的 `auto_binding` 交给 Network 自动异步执行，成功保存凭据、失败保存错误，结果通过 `service()` 获取。业务保存返回的完整 `endpoint_id/relay_url/relay_token/binding`，下次读取后重新设置恢复连接。不得按请求重建 Workspace、查找全局 Network 或维护多个 Network 目标。
- `BindingState` 使用 `Unbound` / `Binding` / `Failed { error }` / `Bound { credential }` 数据 Union，凭据由电脑绑定接口签发并用于每次业务请求，不能以业务布尔值或绑定时间代替。只有绑定接口允许不携带凭据；状态查询也必须验证身份和 token。
- `ResponsError` 是直接返回的数据 Union，分为未设置、未绑定、网络、取消、Services 和参数错误，各分支分别定义 reason。ArkTS 请求采用成功/错误回调，直接返回 Union，不包装成 Error 类。
- `network.rs` 提供单目标通信与绑定；`pairing.rs` 校验邀请；`error.rs` 定义错误；`workspace.rs` 提供实例分发和生命周期；`cpp/workspace.cpp` 只提供 NAPI 桥接。完整配置供业务存储，凭据不进入 UI、错误、监控或日志，Rust Debug 隐藏凭据。
- `Workspace.stop()` 先同步通知网络关闭，再等待未完成请求取消回调并释放 runtime；NAPI 取消和停止信号不等待异步工作线程。停止须幂等，停止后不隐式重新打开。
- 手机绑定时上报在设置页编辑的本机显示名称和 `device_type: mobile`。电脑状态通过 `request(GET /network/session)` 查询，包含只读 name 和 `device_type: computer`；名称和类型只用于展示，手机为固定 mobile、services 为固定 computer，UI 不提供设备类型编辑；services 不提供名称修改接口。
- 界面只显示离线、绑定中、绑定失败、在线。只轮询当前选中电脑，绑定中每轮结束后间隔 1 秒读取 Service，其他情况每轮结束后 20 秒刷新；后台停止。失败错误来自 Service 的 ResponsError，提示删除后重新扫码绑定；删除当前记录清除 Network，保存失败保留原记录。不存在待确认状态或旧邀请重试恢复；自动绑定结束后清除一次性 `auto_binding`，失败结果持久化后不自动重试。

- `scripts/build-device-network.sh` 使用 Rust `aarch64-unknown-linux-ohos` 与 DevEco NDK 构建 ARM64 `liblokiworkspace.so`。`make test` 测试真实应用层代码与 Rust 网络模块；services 的真实 relay 测试调用手机接口，验证凭据和 HTTP 分发。
- `bundleName` 为 `cc.loki.assistant.mobile`。
- 应用、Ability 和启动窗口图标统一引用 `$media:app_icon`；`AppScope/resources/base/media/app_icon.png` 由根目录 `make export-icons` 从 `resources/icons/` 公共母版导出，不手工编辑。
- 使用 DevEco Studio 内置 Hvigor 构建；常用操作统一维护在本目录 `Makefile` 和根目录 `Makefile` 中。
- 构建产物、Hvigor 缓存和依赖目录不提交到仓库。
- `signing/` 管理 `OrgTasksDebug` 公共证书、Loki 专用调试 Profile 和签名配置；私钥放在忽略的 `.signing/`，密码从 macOS Keychain 读取。
- `make build` 先检查 SDK 并安装 OHPM 依赖，显式指定 Debug 模式与可调试标记，通过 Hvigor 的 `PackingCheck` 和 `CollectDebugSymbol` 打包，再由 `scripts/sign-hap.cjs` 独立签名；安装入口只使用已签名 HAP。
- `install`、`run`、`deploy` 统一通过 `scripts/select-hdc-target.sh` 选择设备；默认选择唯一已连接真机。`install` 和 `run` 可显式指定模拟器，`deploy` 始终要求真机并顺序完成构建、安装、启动。

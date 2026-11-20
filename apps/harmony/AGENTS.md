## 职责

`apps/harmony/` 承载 Loki Assistant 的 HarmonyOS 手机执行中心。应用通过 Stage 模式 ArkTS 提供手机端入口，通过 iroh 与电脑完成扫码绑定和状态查询；HTTP 仍只在电脑本机提供。

## 约定

- `entry/src/main/ets/entryability/EntryAbility.ets` 是应用入口 Ability。
- `entry/src/main/ets/pages/Index.ets` 是深色设备入口，使用 Scan Kit 扫码；多设备使用 Swiper 单页展示，左右滑动同步选择唯一 Network 目标，设备概览固定、监控区域独立滚动；`pages/Settings.ets` 是设置页，负责手机名称编辑，首页仅保留设置入口；`ets/app/Application.ets` 在应用层管理电脑列表、手机名称、目标选择、待确认绑定与完整配置持久化，不保留独立 devices 管理模块。
- Rust `Workspace` 创建 Tokio runtime 与唯一 `Arc<Network>`，不从应用数据中自动选择目标。`storage.rs` 只解释本机随机身份，其余 JSON 由应用层解释，沿用私有 `devices.json`、`0600` 权限与原子替换；损坏禁止覆盖，私钥不向 ArkTS 导出。
- `Network` 只设置一个 `Service`，直接提供 `bind()`、`service()` 和异步 `request()`。业务保存返回的完整 `endpoint_id/relay_url/relay_token/binding`，下次读取后重新设置恢复连接。不得按请求重建 Workspace、查找全局 Network 或维护多个 Network 目标。
- `BindingState` 使用 `Unbound` / `Bound { credential }` 数据 Union，凭据由电脑绑定接口签发并用于每次业务请求，不能以业务布尔值或绑定时间代替。只有绑定接口允许不携带凭据；状态查询也必须验证身份和 token。
- `ResponsError` 是直接返回的数据 Union，分为未设置、未绑定、网络、取消、Services 和参数错误，各分支分别定义 reason。ArkTS 请求采用成功/错误回调，直接返回 Union，不包装成 Error 类。
- `network.rs` 提供单目标通信与绑定；`pairing.rs` 校验邀请；`error.rs` 定义错误；`workspace.rs` 提供实例分发和生命周期；`cpp/workspace.cpp` 只提供 NAPI 桥接。完整配置供业务存储，凭据不进入 UI、错误、监控或日志，Rust Debug 隐藏凭据。
- `Workspace.stop()` 先同步通知网络关闭，再等待未完成请求取消回调并释放 runtime；NAPI 取消和停止信号不等待异步工作线程。停止须幂等，停止后不隐式重新打开。
- 手机绑定时上报在设置页编辑的本机显示名称和 `device_type: mobile`。电脑状态通过 `request(GET /network/session)` 查询，包含只读 name 和 `device_type: computer`；名称和类型只用于展示，手机为固定 mobile、services 为固定 computer，UI 不提供设备类型编辑；services 不提供名称修改接口。
- 只轮询当前选中电脑，前台每轮完成后间隔 20 秒刷新；后台停止安排轮询。绑定前保存待确认邀请，响应丢失时在邀请有效期内通过绑定接口重试取得凭据；不使用无凭据状态查询恢复。旧记录缺少凭据时保留身份并重新扫码。已解绑设备提示删除后重新扫码添加，删除入口只清除应用层记录；删除当前设备时清除 Network 目标，保存失败保留原记录。
- `scripts/build-device-network.sh` 使用 Rust `aarch64-unknown-linux-ohos` 与 DevEco NDK 构建 ARM64 `liblokiworkspace.so`。`make test` 测试真实应用层代码与 Rust 网络模块；services 的真实 relay 测试调用手机接口，验证凭据和 HTTP 分发。
- `bundleName` 为 `cc.loki.assistant.mobile`。
- 应用、Ability 和启动窗口图标统一引用 `$media:app_icon`；`AppScope/resources/base/media/app_icon.png` 由根目录 `make export-icons` 从 `resources/icons/` 公共母版导出，不手工编辑。
- 使用 DevEco Studio 内置 Hvigor 构建；常用操作统一维护在本目录 `Makefile` 和根目录 `Makefile` 中。
- 构建产物、Hvigor 缓存和依赖目录不提交到仓库。
- `signing/` 管理 `OrgTasksDebug` 公共证书、Loki 专用调试 Profile 和签名配置；私钥放在忽略的 `.signing/`，密码从 macOS Keychain 读取。
- `make build` 先检查 SDK 并安装 OHPM 依赖，显式指定 Debug 模式与可调试标记，通过 Hvigor 的 `PackingCheck` 和 `CollectDebugSymbol` 打包，再由 `scripts/sign-hap.cjs` 独立签名；安装入口只使用已签名 HAP。
- `install`、`run`、`deploy` 统一通过 `scripts/select-hdc-target.sh` 选择设备；默认选择唯一已连接真机。`install` 和 `run` 可显式指定模拟器，`deploy` 始终要求真机并顺序完成构建、安装、启动。

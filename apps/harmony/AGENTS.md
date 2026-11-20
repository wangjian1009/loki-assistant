## 职责

`apps/harmony/` 承载 Loki Assistant 的 HarmonyOS 手机执行中心。应用通过 Stage 模式 ArkTS 提供手机端入口，通过 iroh 与电脑完成扫码绑定和状态查询；HTTP 仍只在电脑本机提供。

## 约定

- `entry/src/main/ets/entryability/EntryAbility.ets` 是应用入口 Ability。
- `entry/src/main/ets/pages/Index.ets` 是首页设备管理入口；`ets/devices/DeviceManager.ets` 封装异步 NAPI 调用，使用 Scan Kit 系统扫码界面，不配置固定电脑或预设中继。
- `entry/src/main/rust/` 维护正式设备绑定客户端、随机身份和电脑记录；`cpp/devices.cpp` 仅提供异步 NAPI 桥接。身份和中继凭据只保存在应用私有目录的 `devices.json`，权限为 `0600`，损坏禁止覆盖，不向 ArkTS 或日志导出。
- Rust `Workspace` 是应用组合根，持有 Tokio runtime 与一个 `Arc<Network>`，在构造 `DeviceManager` 时注入同一实例。`Network` 只管理外部预设的唯一 `Service`，通用请求使用实例 `request()`，不传目标；未设置 service 时通过错误回调报告。不得按 NAPI 请求重建 Workspace、通过静态变量查找 Network 或让业务模块自行创建它。
- `Workspace.stop()` 先禁止业务新操作，再调用 `Network.shutdown()` 主动通过 `Cancelled` 错误回调取消未完成请求，等待回调与设备业务收尾。NAPI stop 在收尾后关闭 runtime；对象释放也执行兜底关闭。停止须幂等，停止后不得隐式重新打开。
- `network.rs` 提供通用 HTTP 语义请求与 iroh 连接；`workspace.rs` 组合模块；`devices.rs` 负责绑定业务；`storage.rs` 保持私有存储。NAPI 持有 Workspace 的引用，EntryAbility 管理 ArkTS Workspace，页面消费其设备模块。
- 扫码按 `loki-assistant/pair/1` 绑定；前台通过 `Network.request(GET /network/session)` 和 `loki-assistant/services/1` 查询选中电脑。历史多条记录只选择一台接入 Network，未选中的不轮询。已绑定不等于在线，重新进入页面先显示待检查；绑定请求发送前保存待确认记录，响应丢失由后续查询恢复。后台不继续安排轮询，不提供独立后台服务。
- `scripts/build-device-network.sh` 使用 Rust `aarch64-unknown-linux-ohos` target 和 DevEco NDK 生成 ARM64 库，供 Hvigor 打包；`make test` 检查手机 Rust 模块，services 的真实 relay 测试同时调用手机客户端入口。
- `bundleName` 为 `cc.loki.assistant.mobile`。
- 应用、Ability 和启动窗口图标统一引用 `$media:app_icon`；`AppScope/resources/base/media/app_icon.png` 由根目录 `make export-icons` 从 `resources/icons/` 公共母版导出，不手工编辑。
- 使用 DevEco Studio 内置 Hvigor 构建；常用操作统一维护在本目录 `Makefile` 和根目录 `Makefile` 中。
- 构建产物、Hvigor 缓存和依赖目录不提交到仓库。
- `signing/` 管理 `OrgTasksDebug` 公共证书、Loki 专用调试 Profile 和签名配置；私钥放在忽略的 `.signing/`，密码从 macOS Keychain 读取。
- `make build` 先检查 SDK 并安装 OHPM 依赖，显式指定 Debug 模式与可调试标记，通过 Hvigor 的 `PackingCheck` 和 `CollectDebugSymbol` 打包，再由 `scripts/sign-hap.cjs` 独立签名；安装入口只使用已签名 HAP。
- `install`、`run`、`deploy` 统一通过 `scripts/select-hdc-target.sh` 选择设备；默认选择唯一已连接真机。`install` 和 `run` 可显式指定模拟器，`deploy` 始终要求真机并顺序完成构建、安装、启动。

## 职责

`apps/harmony/` 承载 Loki Assistant 的 HarmonyOS 手机执行中心。应用通过 Stage 模式 ArkTS 提供手机端入口，后续辅助能力通过 services HTTP 协议接入。

## 约定

- `entry/src/main/ets/entryability/EntryAbility.ets` 是应用入口 Ability。
- `entry/src/main/ets/pages/Index.ets` 是本地身份组网验证页面，通过 `liblokiprobe.so` 异步调用 Rust。
- `entry/src/main/rust/` 编译共享验证协议与 iroh；`entry/src/main/cpp/` 维护最小 NAPI 桥接。共享源码位于 `services/src/modules/network_probe/probe.rs`，公开测试身份和消息不得各自复制一份。
- 原生库通过 `scripts/build-network-probe.sh` 编译 OHOS ARM64 并打入 HAP；`entry/libs/` 与 Rust target 是构建产物，不提交。此阶段只验证前台模拟器，不宣称支持公网打洞、真实设备后台常驻或正式密钥管理。
- `bundleName` 为 `cc.loki.assistant.mobile`。
- 应用、Ability 和启动窗口图标统一引用 `$media:app_icon`；`AppScope/resources/base/media/app_icon.png` 由根目录 `make export-icons` 从 `resources/icons/` 公共母版导出，不手工编辑。
- 使用 DevEco Studio 内置 Hvigor 构建；常用操作统一维护在本目录 `Makefile` 和根目录 `Makefile` 中。
- 构建产物、Hvigor 缓存和依赖目录不提交到仓库。
- `signing/` 管理 `OrgTasksDebug` 公共证书、Loki 专用调试 Profile 和签名配置；私钥放在忽略的 `.signing/`，密码从 macOS Keychain 读取。
- `make build` 先检查 SDK 并安装 OHPM 依赖，显式指定 Debug 模式与可调试标记，通过 Hvigor 的 `PackingCheck` 和 `CollectDebugSymbol` 打包，再由 `scripts/sign-hap.cjs` 独立签名；安装入口只使用已签名 HAP。
- `install`、`run`、`deploy` 统一通过 `scripts/select-hdc-target.sh` 选择设备；默认选择唯一已连接真机。`install` 和 `run` 可显式指定模拟器，`deploy` 始终要求真机并顺序完成构建、安装、启动。

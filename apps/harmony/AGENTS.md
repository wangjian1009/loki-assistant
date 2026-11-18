## 职责

`apps/harmony/` 承载 Loki Assistant 的 HarmonyOS 手机执行中心。应用通过 Stage 模式 ArkTS 提供手机端入口，后续辅助能力通过 services HTTP 协议接入。

## 约定

- `entry/src/main/ets/entryability/EntryAbility.ets` 是应用入口 Ability。
- `entry/src/main/ets/pages/Index.ets` 是执行中心入口；当前不自动连接预设服务，不保留固定身份的组网验证页面、Rust/NAPI 库或构建脚本。
- `bundleName` 为 `cc.loki.assistant.mobile`。
- 应用、Ability 和启动窗口图标统一引用 `$media:app_icon`；`AppScope/resources/base/media/app_icon.png` 由根目录 `make export-icons` 从 `resources/icons/` 公共母版导出，不手工编辑。
- 使用 DevEco Studio 内置 Hvigor 构建；常用操作统一维护在本目录 `Makefile` 和根目录 `Makefile` 中。
- 构建产物、Hvigor 缓存和依赖目录不提交到仓库。
- `signing/` 管理 `OrgTasksDebug` 公共证书、Loki 专用调试 Profile 和签名配置；私钥放在忽略的 `.signing/`，密码从 macOS Keychain 读取。
- `make build` 先检查 SDK 并安装 OHPM 依赖，显式指定 Debug 模式与可调试标记，通过 Hvigor 的 `PackingCheck` 和 `CollectDebugSymbol` 打包，再由 `scripts/sign-hap.cjs` 独立签名；安装入口只使用已签名 HAP。
- `install`、`run`、`deploy` 统一通过 `scripts/select-hdc-target.sh` 选择设备；默认选择唯一已连接真机。`install` 和 `run` 可显式指定模拟器，`deploy` 始终要求真机并顺序完成构建、安装、启动。

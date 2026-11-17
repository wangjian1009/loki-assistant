## 职责

`apps/harmony/` 承载 Loki Assistant 的 HarmonyOS 手机执行中心。应用通过 Stage 模式 ArkTS 提供手机端入口，后续辅助能力通过 services HTTP 协议接入。

## 约定

- `entry/src/main/ets/entryability/EntryAbility.ets` 是应用入口 Ability。
- `entry/src/main/ets/pages/Index.ets` 是主页面，当前保持空白。
- `bundleName` 为 `cc.loki.assistant.mobile`。
- 使用 DevEco Studio 内置 Hvigor 构建；常用操作统一维护在本目录 `Makefile` 和根目录 `Makefile` 中。
- 构建产物、Hvigor 缓存和依赖目录不提交到仓库。

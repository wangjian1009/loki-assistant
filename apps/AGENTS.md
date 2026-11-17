## 职责

`apps/` 承载可独立运行的客户端应用。每个应用有自己的 CMake 工程、README 和测试；服务能力通过 services HTTP API 使用。

当前应用：

- 根目录 `center/`：预留编排目录。
- `apps/center/`：macOS 菜单栏状态入口。
- `apps/harmony/`：HarmonyOS 手机执行中心，目前提供可运行的空白主页面工程。

服务配置界面位于 `emacs/loki-assistant-config.el`；菜单栏设置通过 services 的 Emacs gateway 打开，不保留独立原生编辑器。应用之间不共享旧版 UI 或 Emacs 控制代码；跨应用能力以 services 的 HTTP 元数据和接口为准。

macOS Bundle ID 统一使用 `cc.loki-assistant` 前缀；HarmonyOS `bundleName` 使用 `cc.loki.assistant` 前缀。对应测试 target 追加 `.tests`。

客户端通过 `~/.assistant/service.lock` 的独占文件锁判断 services 是否存活，再读取 `~/.assistant/service` 获取 host、port 和 version，并请求 `/healthz` 确认健康状态。客户端不得只依赖 pfile 中的 PID；服务未持有锁时显示未运行状态。

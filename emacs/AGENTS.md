## 目录职责

- 本目录维护 `loki-assistant` Emacs Lisp 包。
- `loki-assistant.el` 是包入口，提供 `loki-assistant` feature、同名配置组、异步 HTTP 请求和通知发送。
- 每次 HTTP 请求从 services pfile 发现 loopback 地址；不硬编码端口。通知通过通用请求接口发送，使用 `module_id` 选择服务端处理器。
- `loki-assistant-tests.el` 使用临时 pfile 和真实 loopback listener 验证协议与错误反馈，不发送真实系统通知、不改动用户配置。
- `loki-assistant-config.el` 提供 Customize 风格的 services 配置管理；通过 widget 展示动态元数据，完整加载模块实例后统一展示，预检与保存走现有 HTTP API，不复制服务端业务校验。保存时捕获模块、键和值，区分校验和执行保存阶段，显示等待时长与批量进度；等待期间禁用重复操作，保留编辑能力。成功更新保存基线，旧响应不覆盖新的输入；失败保留输入并显示服务端错误。保存使用独立超时，结果未知时暂停再次提交并要求刷新核对；关闭 buffer 清理等待计时器。
- 登录启动由 services 的控制中心模块在配置提交前处理；界面只提交配置，失败时保留输入并显示服务端错误，不直接管理 LaunchAgent。
- `loki-assistant-monitor.el` 提供主动状态上报和统一 buffer 关注；默认 `loki-assistant-monitor-buffer` 为 `(agent-shell-mode)`，通过 mode hook 注册并通过 `POST /emacs/receiver/status` 同步。关闭 `loki-assistant-monitor-mode` 时移除 hook 和定时器，保留 services 最后状态。
- `loki-assistant-monitor-tests.el` 覆盖关注生命周期、状态分组、请求合并、过期响应和失败保留，纳入 `make emacs-test`。
- `loki-assistant-config-tests.el` 覆盖配置界面和 HTTP 流程，纳入 `make emacs-test`。
- 新增变量、函数和 feature 统一使用 `loki-assistant-` 前缀，文件启用 `lexical-binding`。
- 注释、docstring 和面向用户的文本使用中文；按实际功能需要拆分文件。

## 修改后的加载验证

- 修改 Emacs Lisp 后，必须通过 `emacsclient --eval` 在正在运行的 Emacs 中执行 `require` 加载验证，不得仅检查文件内容。
- 从仓库根目录执行 `make emacs-load`：将本目录加入 `load-path`，若已加载则先执行 `unload-feature`，再执行 `(require 'loki-assistant)` 和 `(require 'loki-assistant-config)`，确保加载修改后的源码。
- 所有 Emacs 操作均使用 `emacsclient`，不得使用 `emacs` 或 `emacs --batch`；`--eval` 中的 Lisp 按多行缩进书写。
- 加载失败时修复错误并重新验证；若 Emacs server 不可连接，应明确报告未完成加载验证。
- 仅运行与改动相关的检查，不运行无关的 Mac 或 services 测试。
- HTTP 或通知实现变更后执行 `make emacs-test`，通过现有 Emacs server 运行 ERT，不退出用户 Emacs。

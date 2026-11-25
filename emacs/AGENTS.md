## 目录职责

- 本目录维护 `loki-assistant` Emacs Lisp 包。
- `loki-assistant.el` 是包入口，提供 `loki-assistant` feature、同名配置组、异步 HTTP 请求和通知发送。
- `loki-assistant-command-menu.el` 提供 Hydra 命令菜单，由交互命令 `loki-assistant-command-map` 按需加载。
- 每次 HTTP 请求从 services pfile 发现 loopback 地址；不硬编码端口。通知通过通用请求接口发送，使用 `module_id` 选择服务端处理器。
- `loki-assistant-tests.el` 使用临时 pfile 和真实 loopback listener 验证协议与错误反馈，不发送真实系统通知、不改动用户配置。
- `loki-assistant-config.el` 提供 Customize 风格的 services 配置管理。入口先选择模块，每个模块在独立 buffer 中通过 widget 显示动态元数据和配置。刷新、预检和保存使用现有 HTTP API，在当前模块内执行。保存时捕获模块、键和值，区分校验和执行保存阶段，显示等待时长与批量进度；等待期间禁用重复操作，保留编辑能力。成功更新保存基线，旧响应不覆盖新的输入；失败保留输入并显示服务端错误。保存使用独立超时，结果未知时暂停再次提交并要求刷新核对；关闭 buffer 清理等待计时器。
- 登录启动由 services 的控制中心模块在配置提交前处理；界面只提交配置，失败时保留输入并显示服务端错误，不直接管理 LaunchAgent。
- `loki-assistant-monitor.el` 通过 `POST /emacs/receiver/status` 主动上报 Emacs 运行信息。关闭 `loki-assistant-monitor-mode` 时取消上报定时器。
- `loki-assistant-monitor-tests.el` 覆盖运行信息上报、状态分组、请求合并、过期响应和失败保留，纳入 `make emacs-test`。
- `loki-assistant-config-tests.el` 覆盖配置界面和 HTTP 流程，纳入 `make emacs-test`。
- `loki-assistant-devices.el` 提供 `loki-assistant-devices`，在 Hydra 菜单中使用 `d` 打开。首页仅展示已绑定设备和新增入口，点击后在当前 buffer 展开 services 返回的 PNG 与绑定信息，不生成二维码或显示令牌。仅可见且等待中的邀请查询进度；收起、关闭或卸载清理计时器并取消邀请，迟到的创建响应也须撤销。`loki-assistant-devices-tests.el` 使用临时 HTTP 服务验证列表、展开、图片、解绑、过期响应和清理，纳入 `make emacs-test`。
- `loki-assistant-edna.el` 由包入口在 Org Edna 加载后加载，提供与 Org Tasks Core 一致的星期排程与后继兄弟创建；`loki-assistant-edna-tests.el` 验证星期推进、夏令时、已有任务链、事务回滚、重复完成创建和标题尾号递增，纳入 `make emacs-test`。Edna 动作入口遵循 `org-edna-action/loki-...!` 命名，其余定义使用模块前缀。
- `loki-assistant-doc.el` 通过 org-roam 提供按 ID 查询与全文提交。`loki-assistant-doc-capture-full` 由 Emacs 按目标选择模板和归档路径，完成保存及索引后返回结果。目标 `article` 使用文档模块自己的模板 `p`。
- 新增变量、函数和 feature 统一使用 `loki-assistant-` 前缀，文件启用 `lexical-binding`。
- 注释、docstring 和面向用户的文本使用中文；按实际功能需要拆分文件。

## 修改后的加载验证

- 修改 Emacs Lisp 后，必须通过 `emacsclient --eval` 在正在运行的 Emacs 中执行 `require` 加载验证，不得仅检查文件内容。
- 从仓库根目录执行 `make emacs-load`：将本目录加入 `load-path`，若已加载则先执行 `unload-feature`，再执行 `(require 'loki-assistant)` 和 `(require 'loki-assistant-config)`，确保加载修改后的源码。
- 已加载的 Slack 聊天模块直接重载源码，保留现有缓冲区的按键、草稿、线程和发送状态。
- 所有 Emacs 操作均使用 `emacsclient`，不得使用 `emacs` 或 `emacs --batch`；`--eval` 中的 Lisp 按多行缩进书写。
- 加载失败时修复错误并重新验证；若 Emacs server 不可连接，应明确报告未完成加载验证。
- 仅运行与改动相关的检查，不运行无关的 Mac 或 services 测试。
- HTTP 或通知实现变更后执行 `make emacs-test`，通过现有 Emacs server 运行 ERT，不退出用户 Emacs。

`loki-spawn-next!` 新建后继时递增目标标题末尾的连续阿拉伯数字，保留前缀与补零位宽；没有数字后缀则保留原标题。每次执行都创建新任务，不维护后继关联或查找已有后继；重新完成会再次创建，标题只根据当前目标递增，不扫描其他兄弟任务的编号。

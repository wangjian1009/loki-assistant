## 职责

`apps/center/` 承载 macOS 菜单栏入口，展示控制中心 `services` 的运行状态并承接系统通知。通知点击通过通用导航 URL 回到 services，由业务模块执行具体跳转；会话和语音输入等其他交互能力等待后续重新设计。

## 约定

- macOS 应用使用 SwiftUI 与必要的 AppKit，工程由 `mac/CMakeLists.txt` 管理。
- 菜单栏应用通过 services 的 `/healthz` 查询状态，不直接调用 emacsclient；系统通知点击只回调 services。
- `loki-assistant://notification/show` 接收通用通知，`loki-assistant://notification/activate` 包装本机服务回调路径，由 `UNUserNotificationCenter` 展示并处理点击，不提供额外 HTTP listener。旧 Emacs URL 和元数据保留兼容。
- 带 `notification_id` 的通知通过 `/notification/{id}/receipt` 回报提交结果；每次回执尝试先检查服务文件锁、读取 pfile 并请求健康接口。回执最多尝试 3 次，4xx 停止重试，不因回执失败重发系统通知。旧版不带 ID 的 URL 仍可展示。
- 菜单每次开始 tracking 时从 services 的 `/emacs/status` 读取最新快照，按 `app` 分组展示各应用的 `buffer` 列表；菜单关闭取消请求，不后台轮询。点击使用 buffer 名称请求 `/emacs/activate`，重复名称按服务端返回的第一个匹配项处理。
- 菜单打开、设置、日志和 agent-shell 切换都是用户操作，结果记录到 `~/.assistant/loki-assistant.log.org`；一级标题记录用户操作，二级标题记录本次操作的 HTTP 调用，文件最多保留最近 500 条一级操作。
- “设置”和“日志”请求 services 的 `POST /emacs/apps/open?app=config|logs`；应用加载、日志文件打开、固定 Lisp 和图形 frame 聚焦均属于服务端，不在 Swift 中维护 Emacs 脚本。应用不存在时展示服务端 `app-not-found` 的中文说明。请求前检查服务文件锁和健康状态；失败显示服务端原因，不自动重试。
- 应用启动后不创建 Dock 主窗口；菜单栏菜单显示连接状态和版本，后台自动监控 services；停止时提供灰色启动按钮。

- 点击时重新发现 services 并向解包后的本机回调路径发送一次 POST，不自动重试、不在 App 中实现 Emacs buffer 操作。通知接口仅接收 `app` 应用标识，不支持指定图标。图标按 `mac/Resources/NotificationAppIcons/<app>.png` 命名并随 App 打包，以 PNG 附件展示；已提供 `emacs`。标识仅允许小写字母、数字、`-` 和 `_`；缺省或资源不存在时展示文字通知。

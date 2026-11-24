## 职责

`apps/center/` 承载 macOS 菜单栏入口，展示控制中心 `services` 的运行状态并承接系统通知。通知点击通过通用导航 URL 回到 services，由业务模块执行具体跳转；会话和语音输入等其他交互能力等待后续重新设计。

## 约定

- macOS 应用使用 SwiftUI 与必要的 AppKit，工程由 `mac/CMakeLists.txt` 管理。
- 菜单栏应用仅在打开菜单时通过 services 的 `/status` 查询一次完整状态，不直接调用 emacsclient；系统通知点击只回调 services。
- `loki-assistant://notification/show` 接收通用通知，`loki-assistant://notification/activate` 包装本机服务回调路径，由 `UNUserNotificationCenter` 展示并处理点击，不提供额外 HTTP listener。旧 Emacs URL 和元数据保留兼容。
- 带 `notification_id` 的通知通过 `/notification/{id}/receipt` 回报提交结果；每次回执尝试先检查服务文件锁、读取 pfile 并请求健康接口。回执最多尝试 3 次，4xx 停止重试，重复转交时通过通知 ID 合并处理，已有提交记录时补发回执。旧版不带 ID 的 URL 仍可展示。
- 菜单每次开始 tracking 时从 services 的 `/status` 读取最新快照，展示服务运行状态和网络连接状态；菜单关闭取消请求，不后台轮询。
- 菜单打开、设置等用户操作记录到 `~/.assistant/logs/center.log`，HTTP 调用以缩进行记录在对应操作之后。文件按 SLF4J 文本格式保留最近 500 条操作。
- 「设置」请求 services 的 `POST /emacs/apps/open?app=config`；应用加载、固定 Lisp 和图形 frame 聚焦均属于服务端，不在 Swift 中维护 Emacs 脚本。应用不存在时展示服务端 `app-not-found` 的中文说明。请求前检查服务文件锁和健康状态；失败显示服务端原因，不自动重试。
- 应用启动后不创建 Dock 主窗口；菜单栏菜单显示连接状态和版本，不启动定时轮询；顶部图标保持单色，仅菜单第一项圆点着色：停止或不可达时红色，注册失败、断开或连接中黄色，注册成功和未配置的 `offline` 绿色。托盘不提供 services 启停，不调用 launchctl；服务管理统一交给 Homebrew 和根目录 Makefile。

- 点击时重新发现 services 并向解包后的本机回调路径发送一次 POST，不自动重试、不在 App 中实现 Emacs buffer 操作。通知接口仅接收 `app` 应用标识，不支持指定图标。图标按 `mac/Resources/NotificationAppIcons/<app>.png` 命名并随 App 打包，以 PNG 附件展示；已提供 `emacs` 和 `slack`。标识仅允许小写字母、数字、`-` 和 `_`；缺省或资源不存在时展示文字通知。

## 职责

`apps/center/` 承载 macOS 菜单栏入口，展示控制中心 `services` 的运行状态并承接系统通知。通知点击通过通用导航 URL 回到 services，由业务模块执行具体跳转；会话和语音输入等其他交互能力等待后续重新设计。

## 约定

- macOS 应用使用 SwiftUI 与必要的 AppKit，工程由 `mac/CMakeLists.txt` 管理。
- 菜单栏应用通过 services 的 `/healthz` 查询状态，不直接调用 emacsclient；系统通知点击只回调 services。
- `loki-assistant://notification/show` 接收通用通知，`loki-assistant://notification/activate` 包装本机服务回调路径，由 `UNUserNotificationCenter` 展示并处理点击，不提供额外 HTTP listener。旧 Emacs URL 和元数据保留兼容。
- 带 `notification_id` 的通知通过 `/notification/{id}/receipt` 回报提交结果；每次回执尝试先检查服务文件锁、读取 pfile 并请求健康接口。回执最多尝试 3 次，4xx 停止重试，不因回执失败重发系统通知。旧版不带 ID 的 URL 仍可展示。
- 应用启动后不创建 Dock 主窗口；菜单栏菜单显示连接状态和版本，后台自动监控 services；停止时提供灰色启动按钮。

- 点击时重新发现 services 并向解包后的本机回调路径发送一次 POST，不自动重试、不在 App 中实现 Emacs buffer 操作。`app-icon://<bundle-id>` 通过 `NSWorkspace` 读取原生应用图标并转换为 PNG 附件。

## 职责

`apps/center/` 承载 macOS 菜单栏入口，展示控制中心 `services` 的运行状态并承接系统通知。通知点击当前只激活 Emacs 应用，指定 buffer 的定位后续再实现；会话和语音输入等其他交互能力等待后续重新设计。

## 约定

- macOS 应用使用 SwiftUI 与必要的 AppKit，工程由 `mac/CMakeLists.txt` 管理。
- 菜单栏应用通过 services 的 `/healthz` 查询状态，不直接调用 emacsclient；系统通知点击使用 `NSWorkspace` 激活 Emacs。
- `loki-assistant://emacs/notification` 接收 services 转交的通知数据，由 `UNUserNotificationCenter` 展示并处理点击，不提供额外 HTTP listener。
- 应用启动后不创建 Dock 主窗口；菜单栏菜单显示连接状态和版本，后台自动监控 services；停止时提供灰色启动按钮。

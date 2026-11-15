## 技术路线

- 使用 CMake 3.30 及以上生成 Xcode macOS App 工程。
- 使用 Swift 6、SwiftUI `MenuBarExtra` 和必要的 AppKit。
- macOS 27 的菜单项图标默认隐藏；`MenuBarExtra` 内容必须设置 `.labelStyle(.titleAndIcon)`。
- 最低支持 macOS 14；应用设置 `LSUIElement = YES`，不显示 Dock 图标。
- 通过 `URLSession` 请求 services 的 loopback `/healthz`，不启动外部进程。

## 目录

```text
Sources/
├── App/LokiAssistantApp.swift       菜单栏应用入口
├── App/ServiceStatusModel.swift     services 状态请求和模型
├── App/SystemNotificationController.swift  通知 URL 解码、系统通知与 Emacs 应用激活
└── Features/Dashboard/DashboardView.swift  菜单内容
Tests/ServiceStatusTests.swift       健康响应和状态模型测试
Tests/SystemNotificationTests.swift  通知数据与展示内容测试
```

增加 Swift 源文件时同步修改 `CMakeLists.txt`，不要直接修改生成的 Xcode 工程。

## 技术路线

- 使用 CMake 3.30 及以上生成 Xcode macOS App 工程。
- 使用 Swift 6、SwiftUI `MenuBarExtra` 和必要的 AppKit。
- macOS 27 的菜单项图标默认隐藏；`MenuBarExtra` 内容必须设置 `.labelStyle(.titleAndIcon)`。
- 应用显示名称为“Loki的助理”；Bundle ID、target 与可执行文件名按现有英文标识维护。
- 应用图标使用 `Resources/Assets.xcassets/AppIcon.appiconset`，由 Xcode 生成 `Assets.car` 和 `AppIcon.icns`；单独导出的 `Resources/AppIcon/loki-assistant-v1.icns` 作为交付资产保留。菜单栏使用 20/40 像素单色 PNG 并设置 template 渲染。SVG 母版位于仓库根目录 `resources/icons/`，根目录 `scripts/export-icons.py` 同步导出各应用资源；入口为 `make export-icons`，构建只消费导出资源。
- `Info.plist` 的显示名称、图标名称和版本字段使用 CMake 的 `MACOSX_BUNDLE_*` 值；不要引用本工程未设置的 Xcode `MARKETING_VERSION`、`CURRENT_PROJECT_VERSION`，否则构建时会丢失版本字段。
- Xcode 将菜单栏的 1x/2x PNG 合并为多倍率 TIFF；通过 `Bundle.main.image(forResource:)` 加载并设置 `isTemplate`，传给 `Image(nsImage:)`。独立资源不使用只查找 asset catalog 的 `Image("名称")` 加载方式；资源缺失时回退到系统图标。
- 最低支持 macOS 14；应用设置 `LSUIElement = YES`，不显示 Dock 图标。
- 通过 `URLSession` 请求 services 的 loopback `/healthz`，不启动外部进程。

## 目录

```text
Sources/
├── App/LokiAssistantApp.swift       菜单栏应用入口
├── App/ServiceSettingsClient.swift  通过 services 打开 Emacs 配置界面
├── App/EmacsReportedStatus.swift    托盘菜单读取 Emacs 上报 buffer 状态
├── App/ServiceStatusModel.swift     services 状态请求和模型
├── App/SystemNotificationController.swift  通用通知解码、系统展示与点击回调
├── App/NotificationReceiptClient.swift     通知回执、点击导航、服务发现及有限重试
└── Features/Dashboard/DashboardView.swift  菜单内容
Tests/ServiceSettingsTests.swift    配置界面 gateway 与失败反馈测试
Tests/ServiceStatusTests.swift       健康响应和状态模型测试
Tests/SystemNotificationTests.swift  通知数据与展示内容测试
Tests/NotificationReceiptTests.swift 回执、重试、导航及服务发现测试
Resources/AppIcon/                   导出的 PNG iconset、ICNS 与菜单栏图标
Resources/Assets.xcassets/            系统应用来源图标的标准 AppIcon 资源
Resources/NotificationAppIcons/<app>.png  按应用标识命名的内置通知图标（已提供 emacs）
```

增加 Swift 源文件时同步修改 `CMakeLists.txt`，不要直接修改生成的 Xcode 工程。

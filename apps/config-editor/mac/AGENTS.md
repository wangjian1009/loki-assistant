## 技术路线

- Swift 6、SwiftUI、CMake 3.30 及以上。
- 使用 `URLSession` 调用 services 的 `/meta`、`/data` 接口。
- 使用 `JSONValue` 表示未知结构的配置值，编辑器不实现服务端校验。
- macOS 14 及以上，普通窗口应用，可独立通过 `open` 启动。

## 职责

- `library/service-proxy/`：平台无关的 Rust 服务客户端。Network 处理认证和传输，ServiceProxy 缓存类型化元数据、配置与状态，并发布状态更新。邮件与 Slack 各自声明业务类型及业务逻辑，界面组件由应用维护。

- `library/xbloom/recipes/`：账号配方导出和公开分享读取，通过 `HttpTransport` 对接客户端的 HTTP 实现。
- `library/xbloom/device/`：xBloom Studio 的 BLE 会话、配方编码与冲煮指令，通过 `BleTransport` 对接客户端的蓝牙实现。

各 crate 均可作为 Cargo 路径依赖独立使用。平台相关的 HTTP 与蓝牙实现由调用方维护。命令行入口位于 `apps/xbloom/`，从仓库根目录执行 `make xbloom-test` 验证库与命令行的工作流。

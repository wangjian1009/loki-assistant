## 职责

- `library/xbloom/recipes/`：账号配方导出和公开分享读取，通过 `HttpTransport` 对接客户端的 HTTP 实现。
- `library/xbloom/device/`：xBloom Studio 的 BLE 会话、配方编码与冲煮指令，通过 `BleTransport` 对接客户端的蓝牙实现。

两个 crate 均可作为 Cargo 路径依赖独立使用。平台相关的 HTTP 与蓝牙实现由调用方维护。命令行入口位于 `apps/xbloom/`，从仓库根目录执行 `make xbloom-test` 验证库与命令行的工作流。

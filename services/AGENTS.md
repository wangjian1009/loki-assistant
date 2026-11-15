## 职责

`services/` 承载控制中心的 Rust 服务层，提供本机 HTTP 能力并编排 Emacs、语音和 macOS 平台服务。它继承仓库根目录的约定；实现说明和使用方式写在 `README.org`，跨模块协议写在 `docs/`。

## 范围

当前只实现控制中心所需的服务框架。菜单栏界面属于 `center/`，agent-shell 的会话执行属于 `emacs/`。不得在此目录复制 SwiftUI 界面或实现另一套 agent-shell 会话引擎。

服务只面向本机 loopback。新增 HTTP 能力须使用版本化 JSON API，并在 `docs/control-center-services.org` 更新边界；不要让 HTTP 路由直接依赖 `emacsclient` 的命令行细节。

## 模块划分

服务层采用单一 Cargo crate，按职责划分 Rust module。`main.rs` 是唯一进程入口和组合根；其他模块不自行启动线程、监听端口或读取全局配置。

当前已经建立的目录和职责：

```text
src/
├── main.rs              读取配置、绑定 listener、处理 Ctrl-C
├── lib.rs               crate 模块导出
├── runtime.rs           进程启动配置、启动时间等运行时状态
├── http/
│   └── mod.rs           公共 HTTP server
└── modules/
    └── health/
        ├── mod.rs       健康模块入口和路由挂载
        ├── protocol.rs  健康响应 DTO
        └── routes.rs    GET /healthz handler
```

每个业务模块必须独立拥有自己的交互协议和路由。新增模块时，最大努力将实现限制在一个 `src/modules/<module>/` 目录中：

```text
src/modules/<module>/
├── mod.rs                对外入口，提供 router/state 组装
├── protocol.rs           本模块请求、响应、错误码和版本
├── routes.rs             本模块 HTTP handler 和路由
└── service.rs            本模块用例和领域逻辑（需要时建立）
```

只有确实被多个模块复用的运行设施才能放在模块目录之外。后续按实际实现新增以下模块，不提前建立空目录：

| 模块             | 职责                                                             | 允许依赖                                   |
|------------------|------------------------------------------------------------------|--------------------------------------------|
| `runtime`        | 读取 loopback 启动地址、启停、关闭信号和运行代次                  | 标准库、Tokio                              |
| `http`           | loopback listener、公共 middleware、请求体限制和 server 生命周期 | `runtime`；不拥有业务协议                  |
| `modules/health` | 服务健康检查和基础运行状态                                       | `runtime`、`serde`                         |
| `modules/emacs`  | `emacsclient` gateway、连接检查、会话操作和事件入口              | 本模块 `protocol`、平台进程 trait          |
| `modules/voice`  | 录音、识别、待确认、取消和失败状态机                             | 本模块 `protocol`、平台音频/识别 trait     |
| `modules/configuration` | 未来的配置中心业务能力（尚未建立）                        | 本模块 `protocol`、平台存储 trait          |
| `platform/macos` | launch agent、进程监控、自动重启、音频设备适配                   | macOS API；实现 `emacs`/`voice` 所需 trait |

依赖方向固定为：

```text
main ──> http server + modules registry
http ──> runtime
modules/<module>
├── routes ──> protocol + service
└── service ──> runtime/platform trait
```

`http` 不直接执行 `emacsclient`，业务模块不依赖 SwiftUI 或菜单栏窗口。协议只在所属模块内定义，不建立集中式 `protocol` 目录；跨模块共享字段必须先证明其稳定且确有复用需求。第一步的 `/healthz` 由 `modules/health` 自己定义协议并挂载路由。当前启动配置属于 `runtime`，未来配置中心使用 `modules/configuration`，不得新增顶层 `config.rs`。

## Rust 约定

- 使用 Cargo 管理 crate，源码和注释中的面向用户文本使用中文，代码标识符保留英文。
- 业务模块通过 trait 依赖平台能力，便于在没有 macOS 资源或 Emacs server 时测试。
- 外部进程调用必须分别传递参数，不经 shell 拼接用户输入；超时、退出码和标准错误需要转换为服务层错误。
- 运行时状态和关闭流程由服务层持有；HTTP handler 不执行长时间阻塞操作。
- 新增目录应与实际实现同步，禁止提前建立空模块。

## 检查

在 `services/` 目录执行 `cargo fmt --check` 和 `cargo test`。修改跨模块协议时，同时检查 `docs/agent-shell-control.org` 及调用方；纯文档变更不增加实现测试。

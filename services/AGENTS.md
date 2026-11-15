## 职责

`services/` 承载控制中心的 Rust 服务层，提供本机 HTTP 能力并编排 Emacs、语音和 macOS 平台服务。它继承仓库根目录的约定；实现说明和使用方式写在 `README.org`，跨模块协议写在 `docs/`。

## 范围

当前实现控制中心服务框架、配置中心与 Emacs gateway。菜单栏状态界面属于 `apps/center/`，根目录 `emacs/` 目前仍是预留空目录。不得在此目录复制 SwiftUI 界面或提前建立旧版会话引擎。

服务只面向本机 loopback。新增 HTTP 能力须使用版本化 JSON API，并在 `docs/control-center-services.org` 更新边界；不要让 HTTP 路由直接依赖 `emacsclient` 的命令行细节。

## 模块划分

服务层采用单一 Cargo crate，按职责划分 Rust module。`main.rs` 是唯一进程入口和组合根；其他模块不自行启动线程、监听端口或读取全局配置。

当前已经建立的目录和职责：

```text
src/
├── main.rs              读取配置、绑定 listener、处理 Ctrl-C
├── lib.rs               crate 模块导出
├── services.rs          Services 组合根和模块路由注册
├── runtime.rs           进程启动配置、启动时间等运行时状态
├── http/
│   └── mod.rs           公共 HTTP server
└── modules/
    ├── health/
    │   ├── mod.rs       健康模块入口和路由挂载
    │   ├── protocol.rs  健康响应 DTO
    │   └── routes.rs    GET /healthz handler
    ├── discover/
    │   ├── mod.rs       发现模块入口和路由挂载
    │   ├── model.rs     模块元数据和 API 描述接口
    │   ├── protocol.rs  发现接口响应模型
    │   └── routes.rs    /discover 路由
    ├── emacs/
    │   ├── mod.rs       Emacs 模块入口、配置和发现元数据
    │   ├── protocol.rs  求值、buffer、通知请求响应与协议版本
    │   ├── routes.rs    /emacs 路由
    │   ├── service.rs   可注入进程 gateway 与异步通知处理器
    │   └── tests.rs     gateway 与 HTTP 协议测试
    └── config/
        ├── mod.rs       配置模块入口和路由挂载
        ├── model.rs     配置模块、配置项和检查器元数据
        ├── protocol.rs  配置接口响应模型
        ├── routes.rs    /meta 和 /data 路由
        ├── service.rs   生效值解析、来源、用户配置与变化订阅
        ├── executable.rs  按元数据搜索默认可执行文件
        └── storage.rs   ~/.assistant/config.yml 原子读写
├── platform/
│   └── macos/
    ├── mod.rs           macOS 平台能力导出
    ├── launchd.rs       LaunchAgent plist 和 launchctl 管理
    └── notification.rs  通过 URL scheme 转交 macOS 系统通知
macros/
└── src/lib.rs          #[loki_api] 与 #[loki_config] 声明过程宏
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

| 模块                    | 职责                                                             | 允许依赖                                   |
|-------------------------|------------------------------------------------------------------|--------------------------------------------|
| `runtime`               | 读取 loopback 启动地址、启停、关闭信号和运行代次                 | 标准库、Tokio                              |
| `services`              | 创建并持有所有业务模块，统一注册模块路由                         | `modules/*`、`runtime`                     |
| `http`                  | loopback listener、公共 middleware、请求体限制和 server 生命周期 | `runtime`；不拥有业务协议                  |
| `modules/health`        | 服务健康检查和基础运行状态                                       | `runtime`、`serde`                         |
| `modules/discover`      | 返回完整模块目录、模块列表和单模块元数据                         | `linkme` 静态模块/API 描述切片             |
| `modules/emacs`         | `emacsclient` gateway、buffer 查询和通知分发                    | 本模块 `protocol`、`CommandRunner` trait、配置服务 |
| `modules/voice`         | 录音、识别、待确认、取消和失败状态机                             | 本模块 `protocol`、平台音频/识别 trait     |
| `modules/config`      | 配置模型、配置数据读写和变化订阅                               | 本模块 `model`、`service`、平台存储 trait   |
| `platform/macos`        | LaunchAgent plist、launchctl 安装/卸载/启停/状态管理               | macOS API；不提供业务 HTTP 路由             |

依赖方向固定为：

```text
main ──> runtime ──> Services ──> modules/*
http ──> runtime ──> Services
modules/<module>
├── routes ──> protocol + service
└── service ──> runtime/platform trait
```

`http` 不直接执行 `emacsclient`，业务模块不依赖 SwiftUI 或菜单栏窗口。协议只在所属模块内定义，不建立集中式 `protocol` 目录；跨模块共享字段必须先证明其稳定且确有复用需求。模块描述通过 `linkme` distributed slice 在编译/链接阶段收集，HTTP 请求只读取静态描述并序列化，不调用运行时注册函数。每个 HTTP handler 使用 `#[loki_api(...)]` 声明接口信息；`Services::build()` 只负责路由组合，`RuntimeState` 持有它。当前启动配置属于 `runtime`，配置中心使用 `modules/config`，不得新增顶层 `config.rs`。配置模块的业务代码通过 `ConfigService::module_values`、`value` 和 `subscribe_module` 访问配置及变化。

配置元数据通过 `#[loki_config]` 修饰无参数同步函数，在编译期注册到 `CONFIG_MODULES`。`Executable` 的默认程序名和补充搜索目录由所属模块元数据声明，查找和 `value`/`source` 解析由配置模块实现，客户端不复制路径发现规则。用户设置保存于 `~/.assistant/config.yml`，静态默认值与发现结果不落盘。

`POST /config/validate` 提供只读配置预检，与保存共用 `ConfigService::validate_value`。`Executable` 必须解析到存在且具有执行权限的普通文件，成功后规范化为绝对路径。校验失败禁止更新内存、YAML 与变化订阅；界面只消费服务端结果。文件系统校验测试使用临时文件，不执行待检查的程序。

进程入口使用 `Services::load()` 读取用户文件后组装服务；`Services::build()`、`ConfigService::new()` 与 `from_registry()` 用于无文件实例。配置存储测试必须使用临时目录，不修改真实用户配置。写入成功后才更新内存并广播变化，写入失败应保留原有状态。

`platform/macos::LaunchdManager` 只管理用户级 `LaunchAgent`，通过独立参数调用 `/bin/launchctl`，不经过 shell。测试必须注入 `CommandRunner`，不得在单元测试中操作真实用户的 launchd；真正安装或卸载服务由上层显式触发。

Emacs 通知上报入口为 `POST /emacs/receiver/notification`。默认处理器通过可注入的 `NotificationLauncher` 调用 `/usr/bin/open -g`，将通知交给菜单栏应用展示。通知单元测试必须注入 fake，不调用真实通知 API；点击只激活 Emacs，`buffer` 暂时仅作为元数据保留。

## Rust 约定

- 使用 Cargo 管理 crate，源码和注释中的面向用户文本使用中文，代码标识符保留英文。
- 业务模块通过 trait 依赖平台能力，便于在没有 macOS 资源或 Emacs server 时测试。
- 外部进程调用必须分别传递参数，不经 shell 拼接用户输入；超时、退出码和标准错误需要转换为服务层错误。
- 运行时状态和关闭流程由服务层持有；HTTP handler 不执行长时间阻塞操作。
- 新增目录应与实际实现同步，禁止提前建立空模块。

## 检查

在 `services/` 目录执行 `cargo fmt --check` 和 `cargo test --workspace`。修改跨模块协议时，同时检查 `docs/control-center-services.org` 及调用方；纯文档变更不增加实现测试。

## 职责

`services/` 承载控制中心的 Rust 服务层，提供本机 HTTP 能力并编排 Emacs、语音和 macOS 平台服务。它继承仓库根目录的约定；实现说明和使用方式写在 `README.org`，跨模块协议写在 `docs/`。

## 范围

当前实现控制中心服务框架、配置中心、控制中心登录启动、系统通知模块、只读密码查询、Emacs gateway 与浏览器全文捕获。菜单栏状态界面属于 `apps/center/`，根目录 `emacs/` 提供 HTTP 客户端、通知发送、全文捕获和 Customize 风格的服务配置管理界面。不得在此目录复制 SwiftUI 界面或提前建立旧版会话引擎。

`modules/network/` 维护 `relay_url`、`relay_token` 配置、本地身份、中转注册和通知。通过配置提交前处理器注册候选连接，落盘成功后才切换并通知，落盘失败撤销；启动初始化由 `main.rs` 显式调度，失败不影响本机 HTTP。构造模块不启动任务，没有固定对端或备用 relay，网络状态通过注入的 `StatusService` 发布到 `network/my`，状态 HTTP 接口属于 `modules/status`。

服务只面向本机 loopback。新增 HTTP 能力须使用版本化 JSON API，并在 `docs/control-center-services.org` 更新边界；不要让 HTTP 路由直接依赖 `emacsclient` 的命令行细节。

`modules/password/` 维护本地 `store_dir`、`pass_path` 配置和只读查询协议。业务 HTTP 仅提供 `GET /password/entries` 和 `GET /password/entries/{*file}`，单条文件标识由路径传递，不接受 JSON 查询请求体。条目列表通过 `pass ls` 获取，解密通过固定的 `pass show --` 获取，不直接读取条目内容或调用 GPG；文件系统只用于目录、普通文件和路径边界检查。命令执行经本模块可注入的 `CommandRunner`，不复用 Emacs gateway。HTTP 不接收程序、库目录或任意命令参数，不提供密码写入操作；明文与 pass/GPG 原始错误不进入日志、通知、状态或发现元数据。测试使用临时目录及进程替身，不读取真实密码库。

HTTP 错误统一返回 `api_version`、HTTP 数字 `status` 和 `error.source/code/message`；`source` 仅使用 `services`、`emacs`、`business`，`code`、`message` 必须为非空字符串，客户端按 `code` 判断并显示 `message`，不得新增模块专用错误外壳。



## 模块划分

服务层采用单一 Cargo crate，按职责划分 Rust module。`main.rs` 是唯一进程入口和组合根；其他模块不自行启动线程、监听端口或读取全局配置。

当前已经建立的目录和职责：

```text
src/
├── main.rs              读取配置、绑定 listener、处理 Ctrl-C/SIGTERM
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
    ├── status/
    │   ├── mod.rs       状态中心描述与实例导出
    │   ├── model.rs     状态模块、状态项与递归 schema 元数据
    │   ├── registry.rs  编译期状态结构目录
    │   ├── protocol.rs  状态元数据和数据查询响应
    │   ├── routes.rs    /stats/meta 结构查询与 /status 数据查询
    │   ├── service.rs   实例发布、批量校验与内存状态存储
    │   ├── validation.rs  外部数据按声明 schema 递归校验
    │   └── tests.rs     原子发布、HTTP 和跨模块实例测试
    ├── network/
    │   ├── mod.rs       网络模块描述和导出
    │   ├── config.rs    中转地址和令牌元数据
    │   ├── protocol.rs  网络连接状态值
    │   ├── identity.rs  ~/.assistant/identity.json 安全读写与随机身份生成
    │   ├── relay.rs     可注入的中转注册器与 iroh 连接
    │   ├── service.rs   配置处理、启动注册、连接切换与通知
    │   └── tests.rs     身份、事务、通知与随机端口 relay 测试
    ├── control_center/
    │   ├── mod.rs       控制中心模块描述和导出
    │   ├── config.rs    登录启动配置元数据
    │   ├── service.rs   提交前 LaunchAgent 同步与落盘失败撤销
    │   └── tests.rs     注入 launchctl 替身的提交与失败测试
    ├── emacs/
    │   ├── mod.rs       Emacs 模块入口、依赖注入和发现元数据
    │   ├── config.rs    EmacsConfig、配置项读取方法与静态配置元数据
    │   ├── protocol.rs  求值、buffer、通知请求响应与协议版本
    │   ├── routes.rs    /emacs 路由
    │   ├── service.rs   可注入进程 gateway 与异步通知处理器
    │   └── tests.rs     gateway 与 HTTP 协议测试
    ├── notification/
    │   ├── mod.rs       通知实例导出、路由挂载与发现元数据
    │   ├── protocol.rs  通用通知、回执与状态模型
    │   ├── routes.rs    /notification 状态与回执路由
    │   ├── service.rs   代码发送接口和应用内跳转封装
    │   ├── tracker.rs   通知回执状态、保留期限与容量限制
    │   └── tests.rs     通用发送与共享实例集成测试
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
└── src/lib.rs          #[loki_api]、#[loki_config] 与 #[loki_status] 声明过程宏
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
| `modules/emacs`         | `emacsclient` gateway、buffer 查询和激活、通知转换、全文捕获     | 本模块 `protocol`、`CommandRunner` trait、配置服务、注入的通知服务 |
| `modules/notification` | 系统通知发送、通用跳转封装和回执状态                           | 本模块 `protocol`、`NotificationLauncher` trait |
| `modules/voice`         | 录音、识别、待确认、取消和失败状态机                             | 本模块 `protocol`、平台音频/识别 trait     |
| `modules/status` | 由业务方设置的通用状态存储与查询 | JSON、内存锁；不依赖具体业务模块 |
| `modules/network` | 身份持久化、中转注册和通知 | 配置处理器 trait、注入的通知服务、iroh 适配 |
| `modules/control_center` | 控制中心配置、菜单栏登录启动与失败撤销 | 配置处理器 trait、平台 LaunchAgent 管理器 |
| `modules/password` | 本地 pass 目录配置、条目列表与只读解密 | 本模块 `CommandRunner` trait、配置服务、文件元数据检查 |
| `modules/config`      | 配置模型、配置数据读写和变化订阅                               | 本模块 `model`、`service`、平台存储 trait   |
| `platform/macos`        | 托盘登录启动的 LaunchAgent plist 与 launchctl 管理               | macOS API；不提供业务 HTTP 路由             |

依赖方向固定为：

```text
main ──> runtime ──> Services ──> modules/*
http ──> runtime ──> Services
modules/<module>
├── routes ──> protocol + service
└── service ──> runtime/platform trait
```

`http` 不直接执行 `emacsclient`，业务模块不依赖 SwiftUI 或菜单栏窗口。协议只在所属模块内定义，不建立集中式 `protocol` 目录；跨模块共享字段必须先证明其稳定且确有复用需求。模块描述通过 `linkme` distributed slice 在编译/链接阶段收集，HTTP 请求只读取静态描述并序列化，不调用运行时注册函数。每个 HTTP handler 使用 `#[loki_api(...)]` 声明接口信息；`Services::build()` 负责模块组装、配置处理器注入和路由组合，`RuntimeState` 持有它。当前启动配置属于 `runtime`，配置中心使用 `modules/config`，不得新增顶层 `config.rs`。配置模块的业务代码通过 `ConfigService::module_values`、`value` 和 `subscribe_module` 访问配置及变化。

配置元数据通过 `#[loki_config]` 修饰无参数同步函数，在编译期注册到 `CONFIG_MODULES`。`Executable` 的默认程序名和补充搜索目录由所属模块元数据声明，查找和 `value`/`source` 解析由配置模块实现，客户端不复制路径发现规则。用户设置保存于 `~/.assistant/config.yml`，静态默认值与发现结果不落盘。

Emacs 模块的配置集中在 `config.rs` 的 `EmacsConfig`，构造时接收 `Arc<ConfigService>`，每个配置项对应独立异步方法，例如 `emacsclient_path()`。方法每次调用配置中心并返回带 `value`、`source` 的 `ConfigValue`，不缓存属性值；`EmacsService` 持有 `EmacsConfig` 并通过这些方法读取。配置项键、默认值和搜索目录均由该文件维护。`EmacsConfig::metadata()` 是不依赖实例的静态函数，经 `#[loki_config]` 注册，配置中心与 API 发现一样通过编译期目录获取元数据。

`POST /config/validate` 提供只读配置预检，与保存共用 `ConfigService::validate_value`。`Executable` 必须解析到存在且具有执行权限的普通文件，成功后规范化为绝对路径。校验失败禁止更新内存、YAML 与变化订阅；界面只消费服务端结果。文件系统校验测试使用临时文件，不执行待检查的程序。

进程入口使用 `Services::load()` 读取用户文件后组装服务；`Services::build()`、`ConfigService::new()` 与 `from_registry()` 用于无文件实例。配置存储测试必须使用临时目录，不修改真实用户配置。模块在组装阶段用 `ConfigService::with_change_handler` 注入提交前处理器，完整替换、合并和追加都先等待处理成功再落盘；处理失败返回 `config-change-rejected`，内存、YAML 和订阅均保持不变。处理器通过 `ConfigChange::module_values` 接收本次候选模块快照，可读取旧值，不得递归提交；有外部状态时实现 `rollback`，供处理成功但落盘失败时撤销。写入成功后才更新内存并广播变化，再调用 `committed` 完成连接切换与成功通知；该回调失败只记日志。写入失败应保留原有状态。

`platform/macos::LaunchdManager` 只管理用户级 `LaunchAgent`，通过独立参数调用 `/bin/launchctl`，不经过 shell。测试必须注入 `CommandRunner`，不得在单元测试中操作真实用户的 launchd；托盘的系统设置由上层显式触发。services 自身使用 Homebrew services，由根目录 Makefile 调用 `scripts/homebrew-service.py` 管理，本机 formula 模板为 `services/homebrew/loki-assistant-services.rb.in`；不得恢复 Rust CLI 自管 services LaunchAgent。`modules/control_center` 拥有 `control-center.center_auto_start` 元数据及处理器，在配置提交时调用 `set_login_startup` 同步托盘登录启动，模块构造和服务启动不自动操作 launchd；Emacs 只提交配置。

`Services` 创建并持有 `Arc<notification::NotificationService>`，创建 `emacs::Module` 时通过 `Arc::clone` 注入通知和状态实例。通知模块直接导出 `NotificationService`，发送、状态查询、回执和路由均通过该实例调用，不另设 `Module` 包装或 `service()` 取用入口。`EmacsService` 保存该 `Arc<NotificationService>` 引用，不得自行创建通知服务或平台发送器。Emacs 通知上报入口保留 `POST /emacs/receiver/notification`：本模块负责把可选 buffer 转成 `/emacs/activate` 回调路径，未收到 `app` 或收到 `null` 时补为 `emacs`（图标由 center 按应用标识从内置资源读取），再调用通知服务发送；不在发送期间连接 Emacs。

`modules/notification` 拥有通用 `NotificationRequest`、代码发送接口、应用内跳转封装、回执与状态。它通过可注入的 `NotificationLauncher` 调用 `/usr/bin/open -g`，使用 `loki-assistant://notification/show` 转交 App；将业务提供的本机服务路径包装成 `loki-assistant://notification/activate` 导航 URL，不解释 buffer。通用路由为 `GET /notification/{id}` 和 `POST /notification/{id}/receipt`；旧 Emacs 路由只代理同一份状态。单元测试注入 fake，不发送真实通知。

系统通知先登记随机 `notification_id` 再转交；相同回执幂等，冲突不覆盖，超时只表示未知，记录按访问清理。不得为状态维护另启线程，测试通过 Tokio 虚拟时间验证期限。自定义 Emacs 模块通知处理器不自动纳入系统通知状态。

`POST /emacs/activate` 执行点击后的业务跳转：无 buffer 时通过独立参数打开 Emacs；有 buffer 时通过现有 gateway 定位存活目标并聚焦图形 frame，不自动启动新 daemon，不创建同名空 buffer。App 只解包通用导航 URL 并回调 services，不直接执行 Emacs 激活。

`POST /emacs/apps/open` 以必填查询参数 `app` 选择固定应用入口，当前支持 `config`。应用标识不作为 Lisp 执行；由 `EmacsService` 通过现有 gateway 完成应用加载、图形 frame 选择和聚焦，Mac 客户端只发送业务请求。未知应用或目标 Emacs 缺少对应 feature 时返回 404 `app-not-found`，客户端展示服务端说明。

服务入口在写入 pfile 前获取 `ServiceInstanceLock`，运行期间持有 `~/.assistant/service.lock`，进程退出时释放。文件锁测试使用临时目录，不操作真实用户的服务锁。

## Rust 约定

- 使用 Cargo 管理 crate，源码和注释中的面向用户文本使用中文，代码标识符保留英文。
- 业务模块通过 trait 依赖平台能力，便于在没有 macOS 资源或 Emacs server 时测试。
- 外部进程调用必须分别传递参数，不经 shell 拼接用户输入；超时、退出码和标准错误需要转换为服务层错误。
- 运行时状态和关闭流程由服务层持有；HTTP handler 不执行长时间阻塞操作。
- 新增目录应与实际实现同步，禁止提前建立空模块。

## 检查

在 `services/` 目录执行 `cargo fmt --check` 和 `cargo test --workspace`。修改跨模块协议时，同时检查 `docs/control-center-services.org` 及调用方；纯文档变更不增加实现测试。

网络身份只在缺失时通过系统随机源生成，以 `0600` 权限原子保存；损坏或公私钥不匹配时禁止覆盖。生成身份与注册结果均通过注入的共享通知服务发送，私钥和令牌不得进入通知、日志或 HTTP 元数据。旧的固定测试身份、验证 CLI 和手机验证桥接已删除；真实 relay 测试仅使用本机随机端口与临时身份。

`StatusService` 由 `Services` 创建并共享；状态值由业务模块通过 Rust 接口发布，不在 status 模块内拉取业务值。Emacs 通过业务入口 `POST /emacs/receiver/status` 上报，由 EmacsService 使用注入的共享 StatusService 发布 `emacs/buffers`，网络模块发布 `network/my`；禁止恢复 Emacs 模块私有的状态存储。`/status` HTTP 仅提供 GET 查询，不提供 POST/PUT 发布路由。网络令牌从配置中心读取，地址与令牌都非空才注册；任意一项缺失视为未配置，不读取环境令牌，不创建 Homebrew 服务环境文件。

状态发布方必须在构造时接收并持有 `Arc<StatusService>` 字段，通过 `self.status.publish` / `self.status.publish_many` 调用实例方法。实例只由 `Services` 组合根创建，再经 `Arc::clone` 注入；业务模块不得自行创建独立实例，不使用全局或静态状态。查询路由挂载组合根持有的同一实例。

状态结构由业务模块自己的 `status.rs` 定义，通过 `#[loki_status]` 注册无参数同步 `StatusModuleMetadata` 函数。`modules/status/model.rs` 定义递归 schema，`registry.rs` 仅聚合静态声明，`GET /stats/meta` 返回完整目录；元数据不依赖运行时值或业务实例，不在状态中心硬编码各业务结构，不包含用户身份或令牌。静态声明目录与每个 StatusService 实例持有的运行状态相互独立。

HTTP 业务上报必须调用注入实例的 `publish_many_validated`，按状态元数据严格校验类型、必填、可空、枚举与嵌套结构，拒绝未知状态项和对象字段。校验整个批次后才写入，失败返回 400 `invalid-request` 和字段路径，不能回显提交值，不得改变已有数据或更新时间。Rust 内部发布仍通过持有实例的 `publish` / `publish_many`。

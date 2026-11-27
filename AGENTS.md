## 项目概述

Loki Assistant 是个人使用的助理程序，为手机与电脑协同工作提供支持。

## 项目选型
- 采用 SwiftUI + 必要的 AppKit 开发 Mac 应用
- 通过本机 HTTP 服务提供模块化能力；平台适配按服务层约定实现。

## 文档规范

- 项目中的文档说明、设计决策、注释性段落和面向用户的文本使用中文。
- 代码、命令、路径、协议名、API 名称、字段名和技术专有名词保留其原始英文形式。
- 项目文档统一使用 Org 格式，放置于 `docs/*.org`；不得新增 Markdown 文档。
- UML 使用 LaTeX `TikZ` 与 `tikz-uml` 绘制，与说明紧邻的 `\begin{tikzpicture}` 源码内嵌维护；不保留独立 `.puml` 或 `.tex` 文件。

## 顶层目录职责

- `apps/`：可独立运行的电脑平台客户端应用与命令行程序。
- `harmony/`：HarmonyOS 手机助手。
- `library/`：可由多个应用复用的库。
- `chrome-plugin/`：Chrome Manifest V3 插件，按功能提供浏览器入口。
- `center/`：预留的应用编排与唤起层。
- `services/`：控制中心核心服务及 macOS LaunchAgent 管理。
- `emacs/`：`loki-assistant` Emacs Lisp 包，提供 services HTTP 请求、通知发送和 Customize 风格的服务配置管理。
- `docs/`：需求、架构决策与跨模块协议。
- `resources/`：跨应用共用的资源母版，目前为 `icons/` 下的 SVG 图标与设计预览。
- `scripts/`：跨应用开发脚本；图标导出脚本从公共母版生成各应用构建使用的资源。

各目录的内部结构和开发约定由该目录的 `AGENTS.md` 定义；根文档不维护完整递归目录树。

## 操作入口

- 根目录 `Makefile` 统一提供开发操作，执行 `make help` 查看入口。
- Makefile 编排 `apps/`、`harmony/` 和 `services/` 的构建、测试、运行与清理；Mac 源码列表与构建设置由各应用的 `CMakeLists.txt` 管理。
- 新增常用操作时同步 Makefile 帮助和相关 `README.org`，不在文档中另行维护一套重复的操作流程。
- `make test` 包含 Mac XCTest 与 services workspace 测试。

## 协作约定

- 修改前阅读适用的各级 `AGENTS.md`；子目录约定补充父目录约定。
- `AGENTS.md` 面向开发协作，定义职责、目录结构及实现约束；`README.org` 面向使用者，说明功能、安装、构建与运行。
- 目录随实现按需创建，不提前铺设空目录或未使用的抽象层。
- 使用 OpenPencil 技能维护界面设计，界面设计尽量拆分避免冲突。
- 图标 SVG 母版统一维护在 `resources/icons/`，通过 `make export-icons` 更新各应用目录中的导出资源；构建只消费已导出资源，不手工修改生成文件。
- 当前已实现 services 服务骨架、服务发现、配置模块、LaunchAgent 管理、Mac 菜单栏状态入口和 Emacs 服务配置界面。文档须区分计划能力与已实现能力，不提供尚不存在的构建或安装命令。
- services HTTP 接口以 `docs/control-center-services.org` 和各模块元数据为准。
- 仅运行与改动有关的检查；不为纯文档变更添加实现测试。
- services 网络模块已实现本地身份持久化与按配置注册中转。HarmonyOS 手机端提供首页设备管理、扫码绑定与电脑状态查询，使用持久化随机身份；具体辅助能力、正式跨设备同步和独立后台服务仍按后续阶段规划。

- 修改 services 核心代码、公开接口或 `docs/core-module-interface.org` 前，主代理必须委派给 `core_maintainer`。涉及接口定义变更时须先取得用户授权，维护流程见 `.codex/agents/core_maintainer.toml`。

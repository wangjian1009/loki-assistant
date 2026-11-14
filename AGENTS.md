## 项目概述

Loki Assistant 是个人使用的助理程序，为手机与电脑协同工作提供支持。

## 项目选型
- 采用 SwiftUI + 必要的 AppKit 开发 Mac 应用
- 通过 `emacsclient --eval` 调用 Emacs 插件并获取函数返回值。

## 文档规范

- 项目中的文档说明、设计决策、注释性段落和面向用户的文本使用中文。
- 代码、命令、路径、协议名、API 名称、字段名和技术专有名词保留其原始英文形式。
- 项目文档统一使用 Org 格式，放置于 `docs/*.org`；不得新增 Markdown 文档。
- UML 使用 LaTeX `TikZ` 与 `tikz-uml` 绘制，与说明紧邻的 `\begin{tikzpicture}` 源码内嵌维护；不保留独立 `.puml` 或 `.tex` 文件。

## 顶层目录职责

- `center/`：助理主程序及各平台应用。
- `emacs/`：安装到 Emacs 的插件，封装 agent-shell 控制能力。
- `docs/`：需求、架构决策与跨模块协议。

各目录的内部结构和开发约定由该目录的 `AGENTS.md` 定义；根文档不维护完整递归目录树。

## 操作入口

- 根目录 `Makefile` 统一提供开发操作，执行 `make help` 查看入口。
- Makefile 编排配置、构建、测试、运行与清理；Mac target、源码列表与构建设置仍由所属目录的 CMake 管理。
- 新增常用操作时同步 Makefile 帮助和相关 `README.org`，不在文档中另行维护一套重复的操作流程。
- `make test` 包含 Mac XCTest 与 Emacs ERT；Emacs 测试通过已有 server 执行，失败必须让 make 返回非零状态，不得退出用户 Emacs。

## 协作约定

- 修改前阅读适用的各级 `AGENTS.md`；子目录约定补充父目录约定。
- `AGENTS.md` 面向开发协作，定义职责、目录结构及实现约束；`README.org` 面向使用者，说明功能、安装、构建与运行。
- 目录随实现按需创建，不提前铺设空目录或未使用的抽象层。
- 当前已实现工程骨架、连接检查、agent-shell 会话列表和会话切换；需求发送仍待实现。文档须区分计划能力与已实现能力，不提供尚不存在的构建或安装命令。
- 跨模块接口以 `docs/agent-shell-control.org` 为准；接口变化时同步相关文档与调用方。
- 使用 `emacsclient` 完成所有 Emacs 操作与验证，不直接启动另一套 Emacs 进程。
- 仅运行与改动有关的检查；不为纯文档变更添加实现测试。
- 第一阶段不扩展到手机端、跨设备同步或独立后台服务。

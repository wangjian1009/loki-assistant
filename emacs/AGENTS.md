## 职责

本目录提供安装到 Emacs 的插件，继承根目录约定。

插件封装 agent-shell 会话发现、buffer 切换与需求发送，通过稳定接口供主程序调用。

## 目录结构

```text
lisp/
  loki-assistant.el               插件入口、配置与统一请求函数
  loki-assistant-protocol.el      请求校验、JSON 编解码与错误响应
  loki-assistant-agent-shell.el   agent-shell 会话发现适配
test/
  loki-assistant-protocol-test.el 协议 ERT 测试
  run-tests.el                   make test-emacs 的加载与执行入口
README.org                       安装、配置、调试与使用说明
```

统一请求函数为 `loki-assistant-request`，当前支持 `ping` 连接检查和 `list-sessions` 会话发现，协议见 `../docs/agent-shell-control.org`。

当前已创建 `lisp/loki-assistant-agent-shell.el` 承载会话发现；buffer 切换和需求发送仍待实现。

## 实现约定

- Elisp 文件使用 lexical binding，公开符号以 `loki-assistant-` 为前缀。
- 优先调用 agent-shell 的公开接口；开发适配前先核实已安装版本与接口，不猜测函数名称或依赖 buffer 名称规则。
- 会话标识在对应会话存活期间保持稳定；操作前重新验证目标是否存在，不能把新请求投递到错误会话。
- 协议仅分发约定的操作，不对请求内容执行 `eval`；用户需求作为数据传递。
- 不通过模拟键盘输入或拼接 shell 命令发送需求。
- 区分 buffer 存在、agent 可用、繁忙等状态；无法可靠判断时返回未知状态。
- 预期失败转换为协议错误；内部诊断与协议返回值分开处理。
- 测试经 `emacsclient --eval` 执行，避免影响用户现有会话；不得调用会退出用户 Emacs 的测试入口。
- 常用测试入口为根目录的 `make test-emacs`；执行前重新加载当前源码，失败须通过 Elisp 错误传递非零退出状态。
- 重点验证会话失效、错误目标、请求编码及发送条件，不为纯结构声明添加测试。

## 目录职责

- 本目录维护 `loki-assistant` Emacs Lisp 包。
- `loki-assistant.el` 是包入口，提供 `loki-assistant` feature 和同名配置组；当前仅初始化包骨架。
- 新增变量、函数和 feature 统一使用 `loki-assistant-` 前缀，文件启用 `lexical-binding`。
- 注释、docstring 和面向用户的文本使用中文；按实际功能需要拆分文件。

## 修改后的加载验证

- 修改 Emacs Lisp 后，必须通过 `emacsclient --eval` 在正在运行的 Emacs 中执行 `require` 加载验证，不得仅检查文件内容。
- 从仓库根目录执行 `make emacs-load`：将本目录加入 `load-path`，若已加载则先执行 `unload-feature`，再执行 `(require 'loki-assistant)`，确保加载修改后的源码。
- 所有 Emacs 操作均使用 `emacsclient`，不得使用 `emacs` 或 `emacs --batch`；`--eval` 中的 Lisp 按多行缩进书写。
- 加载失败时修复错误并重新验证；若 Emacs server 不可连接，应明确报告未完成加载验证。
- 仅运行与改动相关的检查，不运行无关的 Mac 或 services 测试。

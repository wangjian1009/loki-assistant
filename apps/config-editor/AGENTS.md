## 职责

本目录实现独立运行的 macOS 通用配置编辑器。应用启动后从 services 的 `/meta` 获取配置模型，再并行读取所有可编辑模块的 `/data/{module_id}`；元数据和实例数据全部成功后一起展示。编辑保存通过 `PUT /data/{module_id}/{key}` 完成，服务器响应更新对应模块的实例缓存。

编辑器只负责通用展示和 JSON 值编辑，不复制 services 的配置校验逻辑。配置数据包含 `value` 与 `source`（`user` 或 `default`），程序路径的发现由 services 完成。配置项使用单行展示名称、当前值与选择按钮；不显示描述、底层字段名和独立默认值文字。默认来源通过当前值的浅色样式区分。

模块选择只切换对应实例缓存；手动刷新重新获取元数据和实例。跨异步操作保存时携带配置项所属模块 ID，避免写入当前新选中的模块。接口错误显示服务端说明并支持重试，不能把数据解析或校验失败统一解释为服务未运行。模型允许注入 `URLSession` 与服务信息文件位置，网络流程测试不依赖真实服务或用户文件。

`Executable` 类型通过系统文件面板选择程序，允许进入 `.app` 包选择内部可执行文件。输入通过 `POST /config/validate` 做服务端预检，失败时标红当前值并通过悬停显示原因。提交时由保存接口重新校验；界面不自行检查程序权限，也不将预检成功当作保存成功。配置值仍以原始 JSON 提交，不把 `source` 包装写回服务。

固定配置模块 `control-center` 的 `center_auto_start` 由配置编辑器同步管理 `cc.loki-assistant.center` 用户级 LaunchAgent，控制菜单栏程序登录启动。

自动启动管理器先检查任务是否注册，已注册时用 `kickstart`，未注册时用 `bootstrap`；禁用不存在的任务不报错。plist 使用 PropertyListSerialization 编码，并配置 Aqua 会话和日志路径。测试注入 launchctl runner，仅写临时目录，禁止操作真实用户 LaunchAgent。

增加 Swift 源文件时同步修改 `mac/CMakeLists.txt`，不要直接修改生成的 Xcode 工程。

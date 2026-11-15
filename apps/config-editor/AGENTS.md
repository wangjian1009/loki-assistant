## 职责

本目录实现独立运行的 macOS 通用配置编辑器。应用启动后从 services 的 `/meta` 获取配置模型，再读取所选模块的 `/data/{module_id}`；编辑保存通过 `PUT /data/{module_id}/{key}` 完成。

编辑器只负责通用展示和 JSON 值编辑，不复制 services 的配置校验逻辑。配置项默认值来自元数据；未设置值时允许用户编辑默认值并显式保存。

固定配置模块 `control-center` 的 `center_auto_start` 由配置编辑器同步管理 `cc.loki-assistant.center` 用户级 LaunchAgent，控制菜单栏程序登录启动。

增加 Swift 源文件时同步修改 `mac/CMakeLists.txt`，不要直接修改生成的 Xcode 工程。

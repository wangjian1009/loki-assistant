## 职责

本目录维护 Loki Assistant 的 Chrome Manifest V3 插件。插件通过本机 services 的固定
`http://127.0.0.1:47321` 地址查询密码和提交浏览器内生成的 Org 文本，不连接 Emacs HTTP 服务。

## 目录约定

- `extension/popup/` 只负责功能菜单和提示。
- `extension/background/` 负责当前标签页读取、功能调度和异步请求。
- `extension/features/<feature>/` 按功能维护页面转换和功能协议；密码查询、搜索、复制、填充、OTP、业务组合与全文保存分别使用独立目录，新增功能不得把代码放回 popup。
- `extension/shared/` 只放跨功能的消息和 services 客户端。
- `extension/features/org-capture/html2org/` 仅保存第三方源码及许可证；构建输出位于
  仓库根目录的 `dist/chrome-plugin/`，不得手工修改；Chrome 只加载该构建目录。

## 文本与安全

- 面向用户的文字、注释和文档使用中文，代码标识符保留英文。
- 页面内容只在浏览器当前标签页内读取；不在 services 或 Emacs 中重新下载网页。
- 不申请 cookies、下载、剪贴板或全部网页访问权限；复制使用扩展页面可用的 `navigator.clipboard`，权限不可用时显示错误。
- services 地址和 API 路径按设计固定在源码中，不增加 token 或设置页。

## 检查

- 修改 JavaScript 后在本目录执行 `npm run build` 和相关 `npm run test:unit`。
- 构建输出由脚本生成，不提交临时测试浏览器数据。

# 前端来源、主题与组件边界

## 权威基线

KeenCode 前端以 ZCode 3.14.3 的固定提交
`29628c9acdb81b703bbd4080c207a0e7ce5e276e` 为源码基线。提交号、复制范围
和许可证记录在 `THIRD_PARTY_NOTICES.md` 与 `third-party/zcode/`；本文件
只描述当前目标树中的路径和适配边界，不把旧参考实现当作设计来源。

| 来源职责 | 目标路径 | 约束 |
| --- | --- | --- |
| 主题令牌、Tailwind v4 入口 | `packages/ui/src/styles.css` | 保留原 `--color-*`、`--ui-font-size`、圆角和动效角色；业务 CSS 不另造色板 |
| 通用控件 | `packages/ui/src/components/ui/` | 保留原 DOM、可访问性语义和变体；业务调用优先复用这些控件 |
| AI 内容展示控件 | `packages/ui/src/components/ai-elements/` | 仅保留目标聊天、工具、代码和结果展示所需组件，来源范围见第三方清单 |
| 国际化 | `packages/ui/src/i18n/` | 使用原 locale 结构；`en-US` 是键形状，`zh-CN` 是默认中文展示 |
| 业务布局 | `packages/ui/src/` | 复制后只做产品边界裁剪与 KeenCode RPC 接线，不以近似截图重写 CSS |

ResourceManager、workspace/file/workbench/group 拖拽以及 composer/preview/telemetry/AI
Elements 附件均以固定提交中的来源文件为基线保留；逐文件数量、适配文件和 SHA-256
核对见 `third-party/zcode/SOURCE-MAPPING.md`。主 renderer 挂载、Rust DTO、RPC 和
lease 接线属于 KeenCode 适配代码，不改变来源 UI 的归属。

## 主题规则

主题由原 ZCode 的 `--color-*` 语义令牌和 `--ui-font-size` 控制，支持
System、Light、Dark 及仓库已有的 Zai 变体。应用界面文字必须使用
`text-ui-xl`、`text-ui-lg`、`text-ui-base`、`text-ui-caption`、
`text-ui-sm`、`text-ui-xs`；代码、Diff、终端内容可以使用各自的数值字体
设置，但周围的标题、标签和控制器仍使用 `text-ui-*`。

界面层不得写主题色字面量、固定视觉 inline style 或第二套字体缩放机制。
门禁实现见 `tooling/scripts/design-system-gate.mjs`。固定来源中确有的字号、颜色、
原生控件和 inline 布局记录在 `third-party/zcode/design-baseline.json`，按完整
SHA256 或精确三行特征放行；新文件和被修改的特征仍严格检查，不以忽略整个 UI
源码来规避检查。

## 产品裁剪

从来源基线引入的文件必须先通过产品边界审核。官方账号、支付、云端服务、
机器人、SSH、WSL、Docker 和浏览器计算机控制不属于 KeenCode 目标界面；
裁剪包括路由、菜单、翻译键、资源入口以及相应的来源清单项。组件同名不等于
功能已接入，验收以 `docs/frontend-acceptance-matrix.md` 的运行证据为准。

2026-10-04 手工验收反馈进一步明确：仅提供编程模式，首次引导只保留记忆偏好，
不询问工作方向或界面模式；常规设置和侧栏菜单也不提供办公模式切换。
模型设置只有自定义供应商列表，不显示内置/自定义分组小标题。
左上角侧边栏切换入口始终使用侧边栏图标，取消默认 Logo 与 hover 图标的交替显示；
保留展开/收起动作、快捷键和 tooltip，折叠侧栏入口采用同一规则。
左下角移除账户头像与 KeenCode 用户名占位，将入口合并为“设置”与下拉菜单；
首项打开完整设置，分组保留主题、语言及桌面缩放快捷操作。设置页另保留返回工作区箭头，
其共享菜单不调用返回回调。来源组件仍为 `WorkspaceSidebarFooter.tsx`，不引入账号状态。
Composer 原有分支选择器仍展示当前分支；KeenCode 工作树入口使用“工作树”和独立图标，
不再用同一分支名冒充第二个分支选择器。当前工作树的目录/分支仍来自 Rust 查询，保留管理菜单。

## 适配规则

- 保留原 ZCode 组件的 DOM 层级、className、键盘行为和主题角色；确需变化时
  记录在对应协议或验收文档中。
- KeenCode 的会话、工具、历史和工作流状态由 Rust host/Journaling 负责，
  React 只持有可丢弃的显示投影和输入草稿。
- 前端窗口连接是 window-bound 的。主进程只转发认证、心跳和消息，不把另一个
  窗口的连接状态当作当前窗口状态。
- 真实接线使用 `ChannelClient` VQL 100-204 的调用、取消、事件和响应语义；
  v4 wire version 为 3，projection snapshot version 为 1。契约细节见
  `docs/protocols/frontend-rpc.md`。
- 工作流定义是 Rust `serde` 负责的 `WorkflowDefinitionV1` JSON 数据，
  不在桌面运行 Node 或 JavaScript 工作流引擎。JSON 定义与来源 UI 的差异见
  `docs/protocols/workflow-definition-v1.md`。
- 浏览器运行时不导入 Electron/Node 宿主模块，也不包含 TypeScript Agent 执行器；
  `packages/services/src/zcode-agent/zcodeAgent.ts` 仅是 Rust host 的 service
  descriptor/RPC 类型。第三方预览库中的 guarded `process`/`node:fs/promises`
  分支只在 file URL/宿主环境被选中，不能视为产品宿主入口。

## 验收边界

浏览器开发服务器只能验证前端打包和静态交互，不能代替 Tauri 窗口验收。
每一项复制、裁剪、RPC、恢复和主题行为必须在验收矩阵中记录命令、环境、
截图或日志证据；未运行的项目保持 `pending`，不得将源码存在误写为行为通过。

# Harness 前端主题来源与适配

本次改写以 DeepSeek Harness 的前端源码为依据，截图只用于检查渲染结果。参考仓库：<https://github.com/deepseek-ai/deepseek-harness>，固定提交为 `d347e703908d0406b7a7ef80e3a0e594d86b2215`。

## 直接复用的源码

`src/styles/harness/` 中的 `base.css`、`design-platform.css`、`gradient-shadow-text.css` 来自参考仓库的 `packages/client/ui-theme/src/styles/`。只将 `body` 主题宿主转换为 KeenCode 的 `:root` / `[data-theme="dark"]`，字体栈、色值、排版变量、阴影数值保持原样。

`src/styles/tokens.css` 将 KeenCode 原有语义令牌映射到这些 `--dsw-*` 变量，因此工作台、设置、菜单、弹窗、消息、资源面板和首次配置页共用同一套深浅色表面。

重建命令（参考源码目录必须检出固定提交）：

```powershell
node scripts/sync-harness-theme.mjs <deepseek-harness源码目录>
git diff -- src/styles/harness
```

脚本通过 `git show <固定提交>:<路径>` 读取原文件，不会把参考工作树的未提交变更混入主题。版权和完整 MIT 许可保存在根目录 `THIRD_PARTY_NOTICES.md`。

## 组件参数对照

下表的参考路径均相对于 `packages/client/`。数值按实际 CSS 声明选取，不按 CSS 注释中可能过期的设计稿数值估算。

| 区域 | 参考源码 | KeenCode 落点及参数 |
| --- | --- | --- |
| 字体与颜色 | `ui-theme/src/styles/base.css`、`design-platform.css` | `tokens.css`；正文直接使用 `--dsw-font-family`，代码使用 `--ds-font-family-code`，深浅色使用原始语义色 |
| 工作台 / 侧栏 | `ui-layout/src/client/AppFrame.module.css`、`ui-sidebar/src/client/SidebarRoot.module.css` | `app-foundation.css`；默认侧栏 280px、顶部 6px + 品牌行 60px + 行后 8px，新建按钮 38px 高、12px 圆角、8px/16px 内边距 |
| 欢迎标题 | `ui-conversation/src/client/skeleton/HeroShell.module.css` | `MainStage.tsx` / `app-conversation.css`；26px/32px、字重 500、图标 34px、间隔 10px |
| 正文与输入宽度 | `ui-conversation/src/client/skeleton/ConversationRoot.module.css` | `useConversationWidth.ts`；正文 `clamp(680px, 列宽 × 0.64, 920px)`，输入卡片在正文宽度基础上增加 32px，受当前可用宽度限制 |
| 输入卡片 | `ui-conversation/src/client/skeleton/InputBar.module.css` | 22px 圆角、顶部 10px、正文 14px/24px、欢迎输入最小 52px、普通输入最小 28px、滚动上限 336px、正文内边距 4px/8px/0/16px，右侧另留 4px |
| 输入工具栏 | 同上 | 内边距 2px/8px/6px、间隔 12px；发送按钮 34px、上移 2px、信息蓝底与白箭头、禁用透明度 0.4 |
| 输入命令菜单 | `ui-input-trigger/src/client/MenuView.module.css` | 与输入卡片同宽、4px 锚点间隔、20px 圆角、4px 内边距、最高 320px；菜单行最小 40px、14px/22px、8px/10px 内边距 |
| 设置容器 | `ui-settings-general/src/client/SettingsRoot.module.css` | 导航宽 188px、条目高 40px；按后续要求改为占满应用窗口，取消参考的 800px 面板限制与 32px 外圆角，导航和正文统一避让 40px 标题栏 |
| 设置行 | `ui-settings-general/src/client/GeneralSection.module.css`、`locale/src/client/LanguageRow.module.css` | 上下内边距 16px、内容间隔 8px、0.5px 分隔线、标签 14px/22px |
| 外观选项 | `ui-theme/src/client/AppearanceRow.module.css` | 现有 ToggleGroup 的 `appearance` 变体；`flex: 1 1 180px`、内边距 20px/32px、圆角 20px、图文间隔 4px，空间不足时自然换行 |
| 模型设置 | `ui-settings-models/src/client/ModelsSection.module.css` | 纵向提供商列表与编辑区；字段标签 12px/18px、字重 500，提示 12px/18px；沿用 KeenCode 模型协议与保存流程 |
| 按钮 / 输入框 | `ui-primitives/src/Button.module.css`、`Input.module.css` | 标准按钮高 36px、圆角 18px（胶囊）、内边距 0/14px、间隔 4px、14px/22px；小按钮高 28px、12px/18px；输入框高 32px、圆角 8px、0.5px 边框、14px/22px |
| 通用菜单 / 弹窗 | `ui-primitives/src/Menu.module.css`、`Modal.module.css` | 菜单圆角 20px、内边距 4px；弹窗圆角 24px、layer-2 表面、prominent 阴影、标题 16px/24px 与字重 500 |
| 消息 / Markdown / 代码块 | `ui-chat/src/client/chat/MessageItem.module.css`、`ui-primitives/src/markdown/` | `lobe-chat.css`；用户气泡圆角 22px、内边距 10px/16px、14px/22px；Markdown 正文 14px/24px、标题使用原始排版变量，代码块圆角 12px、头部内边距 9px/14px、代码内边距 16px |

字体完整栈：

```css
-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC',
'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif
```

## 有意保留的产品差异

- KeenCode 品牌、中文文案、Tabler 图标体系和现有业务入口继续使用。未把参考项目的账户、服务端、权限或模型配置逻辑接入桌面应用。
- 已保存的侧栏宽度继续生效；280px 是新布局的默认值，不覆盖用户拖动后的尺寸。本次前后截图沿用相同的 260px 已保存侧栏。
- Windows 自定义窗口保留 40px 标题栏空间；设置作为独立全窗口页面显示，顶部可拖动。返回入口为左侧导航顶部的“← 返回应用”，独立于可滚动目录；内容标题行不再显示关闭图标。正文保留 960px 最大阅读宽度；不超过 680px 时使用既有的下拉式设置导航及左侧返回按钮，满足产品 680×620 最小窗口尺寸。
- 壁纸、终端字体设置、Git 状态色和文件类型色保留。其他配色皮肤只改变强调色，不再覆盖 Harness 的表面颜色。
- 保留键盘焦点环与现有 shadcn/ui / Radix 控件语义。欢迎标题移入输入区域正常流；输入高度由 CSS 管理，删除两处旧的 22px × 10 行硬上限。
- 弹窗中的具体表单和额外设置项按 KeenCode 业务内容排版，不能将两款不同产品的整页像素差解读为复制误差。

## 验证与资源影响

本次验证见根目录 `design-qa.md` 的 2026-09-07 记录，包括固定视口的前后像素差、浏览器计算样式、各设置页、最小窗口与 Windows 原生交互证据。

未增加 npm 依赖、字体文件或远程字体请求，未修改 Rust 或 ACP 协议。新增的 ResizeObserver 每个工作台实例只有一个，只在尺寸变化时更新 CSS 变量，卸载时断开，不触发 React 状态更新，也不持续轮询。本次没有重新测量安装包大小、冷启动、空闲 CPU 或进程内存，因此不据此声明性能预算达标。

## 2026-09-07 对话轮次统计

直接适配 Harness d347e703 的 TurnUsagePanel 与 token-format 源码，使用统一 stat Button 和 shadcn/Radix Popover。每轮最终回复显示用量、用时，所有轮次统一 hover/focus 显示（按用户补充要求取消最新轮常显）；明细区分输入、输出、推理、缓存读取和写入。数值来自持久 Journal，不使用上下文占用代替本轮用量。详细源码基线、原生截图和验证边界见仓库根 design-qa.md 的同日记录。

## 2026-09-07 输出速度

已核实 Harness turn-metrics.ts、event-projection.ts、assistant.ts 和 message-chrome.ts 的 TPS 口径：具备输出量与计时的请求累计输出 Token / 累计首段输出至完成耗时；用时弹层按总用时、TPS、首Token延迟排列，>=10 TPS显示整数。输出计时在本机SSE边界采集并随Journal保存。缓存写入Token已从用量明细移除，底层计数保留。来源、公式、原生截图和验证边界见 design-qa.md 同日记录。

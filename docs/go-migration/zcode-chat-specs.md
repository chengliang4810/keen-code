# ZCode 聊天界面逐组件视觉与交互规格（Go 迁移参考）

> 来源：`/Users/chengliang/code-repositories/ZCode` 源码逐文件读取（`packages/ui`、`packages/shared`、`packages/web`），全部结论以「文件:行」引用，行号对应当前检出。
> ZCode 当前聊天界面是 v4 体系：`ConversationTimeline`（虚拟滚动时间线）+ `ConversationTurnGroup`（轮次分组）+ `ConversationRowView`（逐行渲染）+ `ConversationComposer`（输入区）。旧版 `ai-elements`（message/code-block/reasoning 等，源自 vercel/ai-elements，Apache-2.0，见各文件头注释）作为底层原语仍被复用。

## 0. 设计令牌基础（迁移前必读）

字号体系（`packages/ui/src/styles.css:58,143-151`）：

| 令牌 | 值 | 用途 |
| --- | --- | --- |
| `--ui-font-size` | 14px | 全局基准 |
| `text-ui-xl` | base+4px=18px | h1 |
| `text-ui-lg` | base+2px=16px | h2 |
| `text-ui-base` | 14px | 正文/按钮/输入 |
| `text-ui-caption` | 13px | 推荐提示 |
| `text-ui-sm` | 12px | 辅助文本/时间戳 |
| `text-ui-xs` | 10px | 来源徽标 |
| `--font-mono` | 平台等宽栈 + CJK 无衬线兜底（styles.css:140-142） | 代码 |

关键颜色令牌（`styles.css:158-267`，浅色主题；深色主题在 309 行起对称定义）：

- `--color-background` = neutral-50；`--color-surface` = neutral-950 3% 透明混合（:183）；`--color-surface-hover` = 5%（:184）
- `--color-border` = neutral-950 10%（:176）；`--color-border-hover` = 20%（:177）
- `--color-card` = white；`--color-card-border` = border（:186-188）
- `--color-brand` = sky-400（:161）；`--color-icon-blue` = terminal-bright-blue = sky-500（:162,217）
- `--color-foreground` = neutral-700；`--color-foreground-subtle` = 60% 透明混合（:256）；`--color-foreground-subtlest` = 40%（:257）
- `--color-input` = white；`--color-input-border` = border；`--color-input-border-focused` = brand（:194-198）
- `--color-success` = green-600；`--color-warning` = yellow-600；`--color-destructive` = red-600（:259-264）
- `--color-markdown-inline-code` = tag 色（:185）

流式动画工具类（`styles.css:823-842,885-897`）：`animated-gradient-text` 为 300% 宽渐变文字扫光（strong→soft→strong，`gradient-flow` 4s linear infinite，`prefers-reduced-motion` 时各动画关闭，styles.css:879-883,1021-1030）。

## 1. 聊天时间线

### 1.1 时间线容器与消息间距节奏

结构（`packages/ui/src/v4/ConversationTimeline.tsx`）：

- 根：`relative flex min-h-0 flex-1 flex-col`（:1690）；滚动容器 `min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]`（:1744-1749，gutter 稳定预留防止滚动条出现时横向跳动）。
- 虚拟滚动：`@tanstack/react-virtual` 动态测高（ResizeObserver），overscan=8（:97,722-733）；live tail（运行中末轮）不进 virtualizer，独立渲染在消息层末尾（:1855-1882）。
- 消息列宽（`conversationLayout.ts:3-9,13-23`）：草稿态 `max-w-2xl`；会话态 `w-full` → `@min-[864px]` `w-[calc(100%-6rem)] max-w-4xl` → `@min-[1280px]` `w-[calc(100%-24rem)] max-w-6xl`（右侧状态面板展开时同断点生效并整体 `-translate-x-42` 左移，:8-9,40-45）。宽度切换有 `transition-[width,max-width,transform] duration-150 ease-out`（ConversationTimeline.tsx:1820）。

**消息间距节奏**（自外向内）：

- 轮（turn）容器：`relative mx-auto flex w-full flex-col gap-5 px-4 @md/conversation:px-6 pb-5 pt-14`（`ConversationTurnGroup.tsx:1303-1309`）——轮与轮之间 = 上轮 pb-5(20px) + 下轮 pt-14(56px) ≈ 76px；`px-4`（窄）/`px-6`（≥768px 容器）水平内边距。
- 轮内 assistant 流：`flex flex-col gap-5`（ConversationTurnGroup.tsx:873）。
- 连续工具/思考工作项组：`flex flex-col gap-4`（:373-400，注释明确"取代旧版双重不一致间距"）；工具行自身 `py-0`（ConversationRowView.tsx:1940-1943），间距全部由组容器 gap 承担。
- 工时折叠历史展开体：`pt-5`（ConversationTurnGroup.tsx:406-413）；折叠容器用 `[&>*+*:not([data-slot='collapsible-content'])]:mt-5` 保持盒模型（:658-664）。
- 后台结果轮以通知卡开头时去掉 `pt-14` 改 `pt-0`，避免 56px+20px 叠加空白（:1237-1239,1308）。

滚动交互（`ConversationTimeline.tsx`）：

- 底部锚定：跟随中内容变化即 `useLayoutEffect` 吸底（:1618-1656）；用户上滚立即解除跟随（wheel/touch/keyboard capture 意图登记，:821-858, USER_SCROLL_INTENT_TTL_MS=1200 :101）。
- 「回到底部」圆钮：`Button size=icon variant=outline rounded-full bg-card hover:bg-card-selected` + `ArrowDownIcon size-4`（:128-158）；位于 composer dock 上方居中 `absolute bottom-full left-1/2 mb-2 -translate-x-1/2 shadow-sm`（:1932-1944）。
- 历史预取：scrollTop 距顶 2 个视口内自动 `loadOlder`（:1222-1253）；前插后按 virtualizer 锚点平移 scrollTop，阅读位置不跳（:1554-1616）。
- 离底滚动时消息层底部渐隐遮罩：透明区 96px + 渐变 24px（`COMPOSER_MESSAGE_MASK_TRANSPARENT_HEIGHT_PX=96`、`COMPOSER_MESSAGE_MASK_FADE_PX=24`，:99-100,877-912），避免消息从 sticky composer 留白透出。
- 会话切换重置滚动并按 scope 恢复记忆位置（:1438-1500）；草稿态始终展示顶部、真实会话吸底（scrollToBottom 分支，:1036-1049）。

### 1.2 用户消息（UserInputRowView）

来源：`packages/ui/src/v4/ConversationRowView.tsx:846-1303`。

结构（区域自上而下）：附件区 → 气泡 → 状态行 → 操作行，整体右对齐：

- 行壳：`group/user-row flex flex-col items-end`（:1190）。
- 附件区：`mb-2 flex max-w-xl flex-col items-end gap-2`（:1195）；媒体缩略图 `size-20 rounded-xl bg-surface` + 1px 内描边 after（:721-722，行内编辑态缩小为 `size-12 rounded-lg`）；非媒体文件 pill 已发送态 `rounded-full border-0 bg-surface px-3 py-1.5 hover:bg-surface-hover`（:726），编辑态 `h-12 w-fit rounded-lg border border-border bg-surface p-1.5 pr-6`（:725）。文件 pill 内：图标 16px + 文件名 `text-ui-base font-medium text-foreground` truncate + 类型大写标签 `text-ui-sm text-foreground-subtle`（:769-777）。
- 气泡：`flex max-w-full flex-col gap-2 rounded-xl rounded-tr-xs border border-border bg-surface px-4 py-3 text-ui-base text-foreground @min-[624px]/conversation:max-w-xl`（:1254）——右上角收小圆角（`rounded-tr-xs`）是右对齐气泡的方向标记；气泡只装正文，附件/上下文引用在气泡外（:1248-1250 注释）。宿主 ≥624px 时以 36rem 封顶。
- 长正文折叠（`ConversationUserInputBody.tsx`）：折叠限高 **120px**（:7），`overflow-hidden whitespace-pre-wrap break-words transition-[max-height] duration-300 ease-out motion-reduce:transition-none`，折叠且可展开时底部渐隐 mask `linear-gradient(to bottom, black 0%, black 70%, transparent 100%)`（:109-114）；展开按钮为悬浮圆钮 `Button size=sm variant=outline rounded-full bg-background shadow-sm backdrop-blur-sm` + `ChevronDown/Up size-4`，折叠时绝对定位在底部中央，展开时 `pt-2` 常流（:118-141）。
- 引擎尾注（epilogue）：正文截断后折进气泡底部披露块（:862,1265，`ConversationUserInputEpilogue.tsx`）。
- 发送状态行（renderer-local）：`mt-1 text-right text-ui-sm text-foreground-subtlest`，`aria-live="polite"`（:1268-1275）。

操作行（hover 显现）：`MessageActions mt-1 opacity-0 transition-opacity group-hover/user-row:opacity-100 focus-within:opacity-100`（:1279-1283）；手机远控（无 hover）常显（:1277-1278 注释）。含复制（`CopyRowAction`，:164-202：ghost 图标钮，成功后 `CheckIcon size-3.5 text-success` 保持 1200ms）与编辑（铅笔 `size-3.5`，:1290-1299）。

编辑态：整行替换为行内 `ChatPromptEditor`（`w-full max-w-xl`、壳 `min-h-32`，:1164-1165），带取消（Esc tooltip）+ 发送 + 「连同文件回退」（FileClockIcon，outline icon-md，可用性由 fileChanges 状态裁决 :1024-1039）；工作区冲突时弹 rewind 预览 Dialog（:1172-1184）。

### 1.3 助手消息（AssistantTextRowView + MessageResponse）

来源：`ConversationRowView.tsx:1472-1585`、`packages/ui/src/components/ai-elements/message.tsx`。

- 行壳 `group/assistant-row`，正文容器 `w-full text-ui-base` + `data-conversation-selectable`（:1512-1518）。
- 正文渲染走 `MessageResponse`（Streamdown 封装，:1519-1535）：`streaming` 时 `parseIncompleteMarkdown` 容错未闭合 markdown，完成态固定 static 模式（message.tsx:700-706,1632-1635）；**正文不做逐字淡入**（`animated={false}`，message.tsx:1649-1652）。
- 正文排版（message.tsx:1367-1370）：`size-full text-ui-base leading-[1.75] tracking-wide [&>*:first-child]:mt-0 [&>*:last-child]:mb-0`。
- 标题层级（message.tsx:440-447）：h1 `mt-6 mb-4 text-ui-xl font-semibold`；h2 `text-ui-lg font-semibold`；h3/h4 `text-ui-base font-semibold`；h5 `font-medium`；h6 `font-normal`。
- 链接（message.tsx:432-437）：`text-ui-base font-medium text-icon-blue no-underline decoration-dotted underline-offset-4 hover:underline`；工作区文件链接渲染为带 16px 文件图标的按钮（:1102-1104），外链按钮带右键菜单（应用内打开/系统浏览器，:1049-1079）。
- strong → `font-medium`（:1529-1535）。
- 列表（markdown-list.tsx）：ul `my-3 list-outside list-disc space-y-1.5 pl-5 marker:text-foreground-subtlest`（:19-25）；ol `my-3 list-inside list-decimal space-y-1.5 pl-0`（:40-46）；li `pl-1 [&>p]:my-0 [&>p]:inline`（:59）。引用块（markdown-blockquote.tsx:13-17）：`my-4 border-border border-l-2 pl-3 text-foreground-subtle`。
- 行内代码：`rounded-md bg-markdown-inline-code/50 mx-0.5 px-1.5 py-0.5 font-mono text-ui-sm`（message.tsx:1546-1550）。
- 代码块：`my-4 border border-border bg-card`（:1563），头部 `pl-3 pr-2 pt-2`（:1576）；**流式期间禁用语法高亮与 Mermaid**（`enableSyntaxHighlighting={!renderStreaming}`，:1567-1570）。
- Markdown 渲染异常整段降级纯文本（ErrorBoundary fallback，message.tsx:736-747）。

助手操作行（仅轮尾段、complete 态）：`ConversationAssistantTextActions`，`mt-1 opacity-0 transition-opacity group-hover/assistant-row:opacity-100 focus-within:opacity-100`（:1566-1581）。按钮集：复制 / 点赞 / 点踩 / fork / hook 详情 / 创建时间。反馈选中态：赞 `!bg-success/10`、踩 `!bg-warning/10`（:1417,1435）+ `zcode-reaction-burst` 回弹粒子动画（styles.css:1032-1043）；时间戳 `select-none text-ui-sm text-foreground-subtlest`（:1466）。整轮操作栏也可挂在文件摘要之后（`group/assistant-turn` hover 容器，ConversationTurnGroup.tsx:1318-1435, `className="opacity-0 ... group-hover/assistant-turn:opacity-100"` :1429）。

### 1.4 代码块（CodeBlock）

来源：`packages/ui/src/components/ai-elements/code-block.tsx`。

结构：容器 → 头部（标题+操作）→ 内容区。

- 容器：`group relative w-full overflow-hidden rounded-xl bg-background text-foreground` + `contentVisibility: auto`（:152-166）；markdown 场景覆盖为 `border border-border bg-card`（message.tsx:1563）。
- 头部：`flex items-center justify-between gap-3 px-3 py-2 text-ui-base text-muted-foreground`（:188-193）。左侧语言标签 = 文件图标（16px，由语言→文件名映射决定，:73-110）+ 小写语言名 `font-mono truncate lowercase`（:197-199）。
- 操作区：`-my-1 -mr-1 flex items-center gap-1`（:235-243）。按钮均为 `Button size=icon-md variant=ghost`、图标 `size-3.5`：
  - 复制：copy→check 切换，`timeout=2000`ms 复位（:454-512）；
  - 换行切换：`aria-pressed`，激活时 `bg-muted`（:376-409）；
  - Mermaid 预览（仅 mermaid 语言）：Maximize2Icon，SVG 就绪前 disabled（:411-446）。
- 内容区：`p-2 pt-0 pb-3`（:326）；CodeViewer 背景令牌强制对齐 `--diffs-bg: var(--color-card)`（:350-357）；默认字号 `fontSizePx=14`（:253，跟随 codePreviewSettings）。
- 长度限高由调用方经 `contentClassName` 传入（:53-54 注释）。

### 1.5 思考/推理块（Reasoning）

来源：`packages/ui/src/components/ai-elements/reasoning.tsx` + `ConversationRowView.tsx:1587-1620`。

- v4 规则：**流式与完成态都默认收起**，只保留运行态文案，用户可手动展开；`autoCollapseKey`（row 状态）变化自动收起，但用户手动交互后不再自动覆盖（reasoning.tsx:160-178；ConversationRowView.tsx:1595-1598 注释）。
- 触发行：`group/reasoning inline-flex max-w-full min-w-0 items-center gap-2 self-start text-ui-base transition-colors`（reasoning.tsx:362-370）。组成：
  - `BrainIcon size-4 shrink-0 text-foreground-subtlest`（:375，**静态图标**——长流式不旋转以省渲染，:372-374 注释）；
  - 状态文案 `shrink-0 whitespace-nowrap`：流式中 `animated-gradient-text font-medium`「思考中」（:331-334）；完成后 `font-medium text-foreground-subtlest`「已思考」+ `·` + 秒数（≥1s 取整，durationSeconds 计算在 ConversationRowView.tsx:1599-1600；文案结构 :347-359）；
  - **折叠态流式摘要**：取最后一行非空思考文本，单行 `overflow-hidden whitespace-nowrap text-foreground-subtle`，内容增长时把视口推到末尾（旧内容左移），溢出时两侧 16px 渐隐 mask（:268-283,295-327,381-405）；
  - `ChevronRightIcon size-4 text-foreground-subtlest transition-opacity transition-transform`：折叠时 `opacity-0 group-hover/reasoning:opacity-100`，展开 `rotate-90 opacity-100`（:407-413）。
- 内容体：`pt-3` → `max-h-60 space-y-2 overflow-auto text-ui-base text-foreground-subtlest`，default 变体加左导线 `ml-2 border-border border-l pl-3.5`（嵌套在工具组内用 nested 变体去掉导线，:543-550；ConversationRowView.tsx:223,1615）；正文为纯文本 `whitespace-pre-wrap break-words`（性能：不走 markdown，:555-560 注释）。展开时自动吸底跟随最新思考，用户离底后暂停（:451-510）。
- 收起动画：Radix 高度动画 300ms 跑完后才延迟卸载内容（`REASONING_CONTENT_COLLAPSE_UNMOUNT_DELAY_MS=300`，:63,190-207）。
- 空文本流式推理不渲染（ConversationRowView.tsx:1601-1603）。

### 1.6 工具调用卡片（ToolCallBlocks）

来源：`packages/ui/src/ToolCallBlocks.tsx`（入口）、`ToolCallBlocks/ToolLayout.tsx`（骨架）、`ToolSummaryRow.tsx`（摘要行）、`renderers/*`（按工具分流）。

**卡片结构**（无气泡、无边框的「摘要行 + 可展开详情」形态）：

```
[图标] [类别标签] [来源徽标?] [参数摘要...] [状态词] [chevron]
  └─ 展开详情（pt-2，可选）
```

- 行容器：`w-full` + `data-status`（ToolCallBlocks.tsx:369-377）；v4 行级再包 `py-0`（ConversationRowView.tsx:1940-1943），纵向间距由组容器 `gap-4` 统一。
- 摘要行（ToolSummaryRow.tsx:190-222）：`group/tool-summary inline-flex max-w-full cursor-pointer items-center gap-2 self-start text-left text-ui-base transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-input-border-focused`；键盘 Enter/Space 触发展开（:199-208）。
- 摘要行组成：
  - 工具图标：`shrink-0 text-foreground-subtlest`（:73-77），静态不旋转（运行态靠文案扫光，ToolLayout.tsx:139-141 注释）；如终端工具 SquareTerminalIcon `size-4`（renderers/execute.tsx:13）。
  - 类别标签（工具名/类型）：`font-medium whitespace-nowrap shrink-0`，**运行中 → `animated-gradient-text` 扫光，否则 `text-foreground-subtlest`**（ToolLayout.tsx:142-147）。
  - 来源徽标（子代理）：`rounded border border-border bg-background-alt px-1.5 py-0.5 text-ui-xs leading-none text-foreground-subtlest`（ToolSummaryRow.tsx:90-99）。
  - 参数摘要：`min-w-0 flex max-w-full items-center gap-2 text-foreground-subtlest`（:140-146）；命令类摘要收起态用 `font-sans` 与正文一致，展开后详情保留 `font-mono`（execute.tsx:291-296 注释）。
  - 状态词：来自 `getCompactToolCallStatusMessageId`（packages/shared/src/tool-call-summary.ts:38-44）——pending/running/completed/failed/denied/stopped。
  - chevron：`size-4 text-foreground-subtlest transition-transform transition-opacity duration-200 ease-out`，折叠 `opacity-0 group-hover/tool-summary:opacity-100`，展开 `rotate-90 opacity-100`（ToolSummaryRow.tsx:213-220）。
- **运行中状态**：类别标签扫光（见上）；bash 类还有输出预览流式增量（execute.tsx:284-288 `outputPreview` 仅 running 时解析）。
- **错误状态**：`showFailureStatus` → 状态词带虚线下划线 `underline decoration-dotted underline-offset-2 cursor-help`，hover Tooltip（`max-w-96`、`line-clamp-3`）显示错误详情，tooltip 内嵌复制钮（copy→check 1500ms，ToolLayout.tsx:246-288）。失败不再强制展开卡片（:242-244 注释）。
- 展开/收起：
  - open 状态按 toolId 全局持久（会话内记忆，ToolLayout.tsx:16,149-152,307-316）；
  - `autoOpen`：一次性自动展开一次（read/edit 完成）后仍可手动收起（:154-166）；
  - `autoCollapseOnComplete`：running→completed 边沿自动收起（子代理防长摊开，:168-179）；
  - 收起时内容延迟 300ms 卸载，保住 Radix 高度动画（:19,181-210）；
  - 展开详情外壳 `text-popover-foreground outline-none` + 内层 `pt-2`（:20-21,346-357）。
- 流式摘要翻动：摘要内容变化经 `QueuedSummaryContent` 纵向滚播——新摘要 `y:0.8em→0` 淡入、旧摘要 `→-0.8em` 淡出，300ms（ease [0.4,0,0.2,1]）+ 500ms 停留；队列最多 3 格，主线程卡顿时跳播最新（QueuedSummaryContent.tsx:4-12,150-181,234-253；`prefers-reduced-motion` 直接关闭，:31-41）。
- 子工具嵌套：`ml-2 space-y-2 border-border border-l pl-3.5` 左导线缩进（ToolCallBlocks.tsx:24-25）。
- 卡片入场：新工具卡 `data-zcode-tool-stream-animate` → 900ms `cubic-bezier(0.16,1,0.3,1)` 淡入（styles.css:984-988）。

**样例：终端卡**（renderers/execute.tsx:268-380）：类别「终端」/运行中「正在执行」；摘要=命令文本；展开详情 `mb-2 space-y-3 rounded-xl border border-border bg-panel px-4 py-3`，命令行 `$` 前缀 subtle + `pre max-h-15 whitespace-pre-wrap break-words`（:299-307）；输出区 `font-mono text-ui-base leading-5 text-foreground-subtle`，`max-h-[5lh] overflow-auto`（renderers/ExecuteOutput.tsx:27,44）；无输出显示 `chat.toolCall.execute.noOutput`。

### 1.7 工时折叠条（AssistantHistoryStatus）

来源：`ConversationTurnGroup.tsx:565-610`。已完成工作历史的折叠头：`flex w-full border-b border-[var(--color-border)]/50 pb-2`，触发钮 `inline-flex items-center gap-2 text-ui-base text-foreground-subtle`（focus ring 同全局）；文案三态：已停止 / 工作中 N 秒（每秒 tick，ConversationTimeline.tsx:570-583）/ 工作了 N 秒；chevron `size-4 text-[var(--color-foreground-subtlest)] opacity-70 transition-transform`，展开 rotate-90（:597-606）。

### 1.8 系统标记分隔线（compact / fork / modelChange / goalVerify）

来源：`ConversationRowView.tsx:1633-1736`。两侧细线 `h-px min-w-8 flex-1 bg-border/50`，中间 pill `inline-flex items-center gap-1.5 leading-5`：图标 `size-3.5 shrink-0 text-[var(--color-foreground-subtle)]` + 文案 `text-ui-base text-[var(--color-foreground-subtle)]`；行内边距 `px-4 py-2 gap-3`。运行中（压缩进行中）隐藏图标、文案用 `animated-gradient-text font-medium` 流光；fork 分隔线整行可点（button 包裹，`hover:text-[var(--color-foreground)]` + `underline-offset-4 group-hover/marker:underline`，:1703-1720）。

### 1.9 错误横幅（ChatErrorBanner）

来源：`packages/ui/src/ChatErrorBanner.tsx:187-207`。展示于输入区上方、独立于输入 surface（`mb-6 w-full shrink-0`，ConversationComposer.tsx:2216-2228）：

- 壳：`w-full flex flex-wrap items-center gap-2 rounded-xl bg-surface backdrop-blur-md border border-border px-3 py-2`。
- 内容：`InfoIcon size-4 shrink-0`（hook 拦截用 AnchorIcon）+ 摘要 `min-w-0 truncate font-medium text-ui-base text-foreground`。
- 操作（outline sm 按钮）：模型缺失 → 升级（渐变钮）+ 打开模型设置；一般错误 → 「查看详情」（改弹 Dialog，内嵌 `pre max-h-[60vh] overflow-auto rounded-xl border border-border bg-surface px-3 py-2`，:257-269）+「复制完整错误」+ 反馈入口。
- suppression 规则：`shouldSuppressChatErrorBanner` 过滤后 `visibleError` 才渲染（ConversationComposer.tsx:1622）。

### 1.10 流式中的视觉汇总

- **无打字光标**：正文流式只更新文本，无闪烁 caret（`animated=false`，message.tsx:1649-1652）；工作流图另有 `.wf-caret` step-end 闪烁 caret（styles.css:1607-1608,1955-1957），不属于消息正文。
- 运行进度由四类信号表达：
  1. ChatLoading 旋转点：`LoaderIcon animate-spin text-foreground-subtle`，sm `size-4` / 默认 `size-6`，槽位 `min-h-5` 占位（chat-loading.tsx:21-36；ConversationTurnGroup.tsx:156-167）——仅轮次 running 且未被交互阻塞时显示；
  2. 文字扫光：思考中/工具类别标签/后台标题的 `animated-gradient-text`；
  3. 摘要滚播：工具/思考折叠态的最新一行文本滚动（QueuedSummaryContent）；
  4. API 重试计数：第 3 次起才显示（`MIN_VISIBLE_API_RETRY_ATTEMPT=3`，ConversationTurnGroup.tsx:125,150-156）。
- 新内容入场 900ms 淡入（`zcode-stream-text-in`，styles.css:900-908,984-988）。

## 2. 输入区（Composer）

### 2.1 布局与壳

来源：`ConversationComposer.tsx:2189-2299`、`ChatPromptEditor.tsx:335-452`、`ConversationTimeline.tsx:1906-1949`。

- bottom dock：sticky 在滚动容器底部，`pointer-events-none z-20 flex w-full justify-center`，内容列 `pointer-events-auto relative z-10 w-full px-4 pb-4`（ConversationTimeline.tsx:1906-1931）。
- composer 根：`chat-composer-region z-20 w-full shrink-0 @container/composer`（工具条响应式以自身为容器查询），草稿居中态 `max-w-2xl`（ConversationComposer.tsx:2198-2206）。
- 输入壳（ChatPromptEditor）：`relative flex flex-col gap-3 overflow-hidden rounded-2xl border border-input-border bg-input p-3 transition-colors hover:border-input-border-hover focus-within:!border-input-border-focused focus-within:bg-input-focused`（ChatPromptEditor.tsx:346-351）——**hover 使用 input-border-hover、聚焦使用 input-border-focused + input-focused；zai 的聚焦边框映射 border-hover，并非品牌色**。草稿态外包 `rounded-2xl bg-surface shadow-xl/5` 卡（ConversationComposer.tsx:2229-2233）。
- 内部结构：topContent（附件预览/上下文 chips）→ 多行 Lexical 编辑器 → 工具条 `group/toolbar flex items-end gap-3`（左侧 leadingActions 弹性，右侧 trailing `ml-auto flex shrink-0 items-center justify-end gap-1.5`，:388-414）。
- 拖拽悬停：壳变 `border-brand bg-input-focused ring-1 ring-brand/30`，并盖 `bg-accent/55 backdrop-blur-sm` 遮罩 + 居中 pill `rounded-full border border-border bg-accent px-4 py-2 shadow-sm`（Hand 图标 + 提示文案）（:348-360）。
- 附件错误行：`flex items-start gap-2 p-3 text-ui-base text-warning` + InfoIcon（ConversationComposer.tsx:2293-2298）；对话引用超限警告 `mb-2 rounded-lg border border-[var(--color-warning)]/30 bg-[var(--color-warning)]/10 px-3 py-2 text-ui-base text-foreground`（:2239-2248）。

### 2.2 发送 / 停止按钮状态机

来源：`ConversationComposer.tsx:1098-1136,2064-2098`。

- `canSend = !disabled && !pending && hasDraftToSubmit && routingAllowsSend && attachmentsReady && submissionReady`（:1128-1134）——草稿=文本/附件/上下文引用任一存在；attachmentsReady 要求无上传中附件。
- **Stop 优先**：`showStopControl = canStop && !hasDraftToSubmit`（:1136）——运行中且空草稿显示停止；有草稿则显示发送（入队语义）。
- 发送钮：`Button type=submit size=icon-md`，`rounded-lg bg-brand text-ui-base text-foreground-inverse hover:bg-brand/80`，图标 `ArrowUpIcon size-4`；提交中换 `Spinner size-4`（:2085-2096）。disabled 时强制关闭 tooltip（:1138-1140）。
- 停止钮：`variant=secondary size=icon-md`，`SquareIcon size-4 fill-current`，tooltip `chat.stop` + 快捷键 **Esc**（:2064-2077）。
- 发送 tooltip：`ControlHintTooltip` 标题 + 快捷键——Enter 提交；队列模式下标题换「加入队列」；按住修饰键（⌘/Ctrl，按平台）反转投递方向并即时改 tooltip（:1607-1620, followupModeSettings）。
- 队列二次确认 Dialog：`max-w-xl gap-6 p-6 sm:p-8`，标题 `text-xl sm:text-2xl font-semibold`；按钮 `min-w-32 rounded-full`，清队列=destructive，保留队列=pending 时带 Spinner（:2314-2382）。

### 2.3 占位符

来源：`packages/ui/src/lib/chatPlaceholder.ts:15-23` + `ConversationComposer.tsx:1598-1606`。三分支：无历史 →「描述新任务」；有历史空闲 →「继续追问」；有历史且处理中 →「排队追问」。

### 2.4 工具条：模型选择器 / 思考深度 / 模式

来源：`ConversationComposer.tsx:2038-2166`、`v4/composer/V4ComposerToolbar.tsx:963-1089`。

- 左簇（工具条左端）：模式切换（V4ComposerModeSwitch，含 Plan 模式）+ CUA 入口 + 后台任务入口；右簇前为模型/思考/context 用量簇（`flex min-w-0 items-center gap-1`，:2040-2063）。
- 模型选择器（ModelConfigSelect）：
  - trigger `composer-model-trigger max-w-[16rem]`；容器 <`@sm/composer` 只显示图标（`size-7` 压缩），≥`@sm/composer` 显示文本，provider 前缀 ≥`@2xl/composer` 才显示（:1049-1069）；
  - 加载失败 → ghost sm 重试钮 `h-7 px-2 text-ui-sm text-destructive`（:1023-1032）；目标不可用 → `px-2 text-ui-sm text-foreground-subtle` 说明文案（:1033-1041）；
  - 无可选组但有「管理模型」入口时仍显示，防零模型死路（:969-971）。
- 思考深度：ThoughtLevelCycleControl，档位来自目标 Host 模型目录（:1074-1089）。
- **快捷键**：Ctrl+M 打开模型菜单、Ctrl+T 循环思考深度、Ctrl+Shift+M 循环模式（:961-982 注释与绑定；tooltip 文案读命令表）。
- Context 用量面板：ChatContextUsage + Coding Plan 余量（:1013-1022）。
- 禁用：`disabled || recoveryPending` 传导到选择器与热键（:975-982,1058,1080）。

### 2.5 禁用态与阻塞

- 编辑器 `disabled={disabled || mode === "reject"}`，submit 独立判定（pending/路由/附件/配置四门，:2255-2257）。
- 底部交互阻塞（权限/问答卡弹出）：composer 保留挂载但 `aria-hidden` + `display:none`，草稿与编辑器状态不丢（:2185-2207）。

## 3. 空态界面（新会话/草稿）

来源：`packages/ui/src/v4/ConversationDraftEmptyState.tsx`、`ConversationDraftSuggestedPrompts.tsx`、`ConversationTimeline.tsx:1764-1771`。

- 布局：问候语 + composer 作为整体在时间线内居中——容器 `flex min-h-full flex-col items-center px-4`，顶部弹性留白 `before:` 伪元素 `min-h-[52px] basis-[29dvh]`（随视口高度伸缩），底部 `after:min-h-4 flex-1`（ConversationTimeline.tsx:1764-1767）；消息列收窄 `max-w-2xl`（conversationLayout.ts:3,17）。草稿态滚动吸顶不吸底（:1036-1049）。
- 问候语块：`relative mb-10 flex w-full max-w-2xl flex-col items-center justify-center gap-6 text-foreground sm:mb-8`（:170-175）。
- 背景 Logo：绝对居中 `aspect-[5/4] w-[min(72vw,25rem)] -mt-10 text-foreground-subtlest`；浅色为线框 SVG `opacity-70` + 底部渐隐 mask `linear-gradient(to bottom, black 0%, transparent 70%)`，深色用专用渐变位图互斥显示（:177-184,216-244）。
- 问候语文案：按小时分段（5/9/12/14/18/23 点换档，含深夜/清晨/办公模式文案，:14-36）；字号 **20–30px 自适应**（标题自然宽 vs 容器可用宽缩放，`font-medium` 行高 1.2，:15-16,54-78,194-197）。
- 推荐提示（草稿态，位于问候与 composer 之间）：水平滚动条隐藏的一排 `Button variant=outline size=lg`：`h-8 min-w-0 rounded-lg px-3 gap-1.5 text-ui-caption font-normal leading-4.5 text-foreground`（注意 outline lg 默认高被 h-8 覆盖）；图标 `size-4 opacity-70 group-hover:opacity-100`，文本 `max-w-64 truncate text-ui-base`；入场为逐项瀑布淡入 `zcode-draft-prompt-waterfall`，延迟 `index*65ms`（ConversationDraftSuggestedPrompts.tsx:312-340,321-326；keyframes styles.css:910-918）。竖排变体（office）为 `flex w-full items-center gap-3 rounded-xl p-3 hover:bg-hover` 列表 + `size-6 rounded-md bg-surface` 图标座（:253-275）。

## 4. 数据契约（packages/shared，渲染层上游）

- `assistant-message-parts.ts:1-13`：助手消息 = 有序 part 流 `content | thought | tool-call{toolId}`；连续同类文本 chunk 必须合并为同一 part（:15-40），第三方完成消息与 UI latestPart 都依赖该边界。
- `assistant-presentation.ts:18-37,75-151`：part + toolCall 详情合成 blocks 投影；streaming/interrupted/settling 时不产出 latestPart；嵌套工具（parentToolUseId 命中父）不重复渲染（:107-117）。
- `tool-call-summary.ts:21-44,145-222`：工具卡摘要契约——`primaryText`（标题）、`secondaryText`（首个 string 型 command/path/file_path/filePath/prompt 字段，空白归一）、`changeStat`（edit/patch 类统计 before/after 行数得 added/removed）；状态词映射 pending/running/completed/failed/denied/stopped。

## 5. Web 端（packages/web）与只读分享视图

- `packages/web/src/share/` 只含分享落地页（landing page + 预览客户端），**无独立聊天组件**；会话分享只读时间线在 `packages/ui/src/v4/ConversationShareReadonlyTimeline.tsx`，复用与主界面相同的行样式：用户气泡同款 `rounded-xl rounded-tr-xs border border-border bg-surface px-4 py-3`（:235）、附件 pill `rounded-lg border border-border bg-surface px-3 py-1.5 text-ui-sm`（:221）、助手正文同 `MessageResponse`（:266-267）、工件卡 `rounded-xl border border-card-border bg-card p-3 pr-4` + `size-11 rounded-md bg-background` 图标座（:473-498）。

## 6. Go/UI 迁移要点（对照清单）

1. 间距节奏只有三个数：轮间 pt-14/pb-5、轮内 gap-5、工作项 gap-4（工具行自身零 padding）；不要引入每行 py。
2. 用户消息右对齐方向角 `rounded-tr-xs`；助手正文无气泡无背景。
3. 三类折叠（用户长文/思考/工具详情）都遵循「300ms 高度动画 + 延迟卸载内容」与「chevron hover 才出现、展开 rotate-90」的同构交互；思考与工具默认收起。
4. 运行态视觉 = 静态图标 + 文字扫光 + ChatLoading 自旋点，不使用旋转工具图标。
5. 发送键是状态机不是按钮组：空草稿+running=Stop(Esc)，否则 Send(Enter)，队列模式下标题/行为随修饰键翻转。
6. 所有颜色/字号/圆角必须落在本节令牌表（styles.css @theme）上；严禁硬编码 hex/px 字号。

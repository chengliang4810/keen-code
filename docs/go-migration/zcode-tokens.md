# ZCode 设计令牌精确提取（Go 原生 UI 还原用）

> 来源：`/Users/chengliang/code-repositories/ZCode`（Apache-2.0）。本文所有数值照抄源码；带「换算」标记的 hex 是 Tailwind 官方调色板 OKLCH 值按 CSS Color 4 规范换算的 sRGB 值（浏览器实际渲染值），其余 hex 均为源码字面值。

## 0. 主题机制（Go 端需要复刻的前置事实）

- 主题共 4 面 + system：`light` / `dark` / `zai-light` / `zai-dark` / `system`。`dark`、`light` 会被归一化为 `zai-dark`、`zai-light`（`packages/web/src/webThemeSeed.ts:5-24`、`packages/ui/src/useTheme.ts:31-36`）。
- 应用方式：在 `<html>` 上切换 `dark` class + `theme-zai-light` / `theme-zai-dark` class（`packages/ui/src/useTheme.ts:44-56`）。Go 端对应：基础变量取 `:root`/`.dark` 值，再用 zai 两套变量整体覆盖。
- CSS 级联：`:root`(`@theme`，默认 light) → `.dark` 覆盖 → `.theme-zai-light` / `.theme-zai-dark` 再覆盖。zai 主题未重定义的变量（如 `--color-workflow-*` 之外的多数图表色在 zai 下均有定义）回落到默认主题值。
- Web 端默认主题是 **zai-dark**；`localStorage["zcode-theme"]` 持久化；分享页无配置时用 zai-light（`packages/web/src/webThemeSeed.ts:5`、`packages/web/src/main.tsx:28-48`）。桌面端 useTheme 兜底同为 zai-dark（`packages/ui/src/useTheme.ts:71`）。
- `system` 模式跟随 `prefers-color-scheme`，解析后仍落到 zai 两面（`packages/ui/src/useTheme.ts:14-16,44-51`）。
- 桌面磨砂玻璃：`html/body/#root` 背景 `transparent !important`（`packages/ui/src/styles.css:37-47`）；`#root` 高 `100dvh`（styles.css:108-113）。
- 根字号变量 `--ui-font-size: 14px`（styles.css:57），界面字号设置只改它，不改根 rem。
- 注意：源码中引用了 `var(--color-ring)`（styles.css:1474、`components/ai-elements/connection.tsx:17,20`）但全仓库未定义该变量（本次已检索确认），属上游遗留，Go 端无需实现。

## 1. 核心颜色令牌 — 默认主题（`:root` 亮 / `.dark` 暗）

来源：`packages/ui/src/styles.css:137-306`（light）、`308-459`（dark）。基础引用写法保留 + 换算 hex。`color-mix(in oklab, X n%, transparent)` 按 CSS 规范等价于「同色 alpha=n%」。

### 1.1 背景层级

| 令牌 | 亮色值 | 暗色值 | 用途 |
| --- | --- | --- | --- |
| `--color-background` | neutral-50 `#FAFAFA` | neutral-900 `#171717` | 应用主背景 |
| `--color-background-alt` | `color-mix(oklab, neutral-100 60%, transparent)` ≈ `#F5F5F5@60%` | `color-mix(oklab, neutral-800 60%, transparent)` ≈ `#262626@60%` | 备用背景（styles.css:159/312） |
| `--color-background-win-alt` | neutral-200 `#E5E5E5` | neutral-800 `#262626` | 窗口备用背景 |
| `--color-header` | neutral-100 `#F5F5F5` | neutral-900 `#171717` | 顶栏 |
| `--color-panel` | neutral-100 `#F5F5F5` | neutral-900 `#171717` | 面板 |
| `--color-sidebar` | neutral-100 `#F5F5F5` | neutral-950 `#0A0A0A` | 侧栏 |
| `--color-surface` | `neutral-950 3%` ≈ `#0A0A0A@3%` | `white 5%` ≈ `#FFFFFF@5%` | 表面/行底 |
| `--color-surface-hover` | `neutral-950 5%` | `white 10%` | 行悬停 |
| `--color-card` | white `#FFFFFF` | neutral-800 `#262626` | 卡片 |
| `--color-card-selected` | neutral-200 `#E5E5E5` | neutral-700 `#404040` | 卡片选中 |
| `--color-card-border` | =`--color-border` | =`--color-border` | 卡片描边 |
| `--color-popover` | white `#FFFFFF` | neutral-800 `#262626` | 浮层/弹出 |
| `--color-popover-foreground` | =`--color-foreground` | =`--color-foreground` | 浮层文字 |
| `--color-popover-header` | neutral-100 `#F5F5F5` | neutral-700 `#404040` | 浮层头 |
| `--color-popover-border` | =`--color-border` | =`--color-border` | 浮层描边 |
| `--color-input` | white `#FFFFFF` | neutral-800 `#262626` | 输入框底 |
| `--color-input-focused` | neutral-50 `#FAFAFA` | neutral-950 `#0A0A0A` | 输入框聚焦底 |
| `--color-tab` | neutral-100 `#F5F5F5` | neutral-800 `#262626` | 标签页底 |
| `--color-tab-active` | white `#FFFFFF` | neutral-950 `#0A0A0A` | 激活标签页 |
| `--color-menu` | white `#FFFFFF` | neutral-950 `#0A0A0A` | 菜单 |
| `--color-menu-hover` | neutral-100 `#F5F5F5` | neutral-900 `#171717` | 菜单悬停 |
| `--color-toast` | white `#FFFFFF` | neutral-900 `#171717` | 通知条 |
| `--color-tooltip` | neutral-100 `#F5F5F5` | neutral-900 `#171717` | Tooltip 底 |
| `--color-tooltip-tag` | neutral-200 `#E5E5E5` | neutral-800 `#262626` | Tooltip 键位标签底 |

### 1.2 前景层级

| 令牌 | 亮色值 | 暗色值 | 用途 |
| --- | --- | --- | --- |
| `--color-foreground` | neutral-700 `#404040` | neutral-200 `#E5E5E5` | 主文字 |
| `--color-foreground-subtle` | `neutral-700 60%` ≈ `#404040@60%` | `neutral-200 60%` ≈ `#E5E5E5@60%` | 次文字 |
| `--color-foreground-subtlest` | `neutral-700 40%` ≈ `#404040@40%` | `neutral-200 30%` ≈ `#E5E5E5@30%` | 弱文字 |
| `--color-foreground-inverse` | white `#FFFFFF` | white `#FFFFFF` | 反色文字 |
| `--color-primary` | neutral-950 `#0A0A0A` | neutral-50 `#FAFAFA` | 主按钮底 |
| `--color-primary-foreground` | neutral-50 `#FAFAFA` | neutral-950 `#0A0A0A` | 主按钮文字 |
| `--color-secondary` | neutral-300 `#D4D4D4` | neutral-700 `#404040` | 次级控件底 |
| `--color-tooltip-foreground` | neutral-950 `#0A0A0A` | neutral-50 `#FAFAFA` | Tooltip 文字 |
| `--color-tooltip-tag-foreground` | `neutral-950 60%` ≈ `#0A0A0A@60%` | `neutral-50 60%` ≈ `#FAFAFA@60%` | 键位标签文字 |
| `--color-tag` | neutral-200 `#E5E5E5` | neutral-700 `#404040` | 标签底（亦=行内代码底） |
| `--color-muted-*` | 未在本仓库定义 | — | （shadcn 包装组件偶用 `text-muted-foreground`，仅 input-group.tsx:26 一处包装层，Go 端可映射 foreground-subtle） |

### 1.3 边框

| 令牌 | 亮色值 | 暗色值 | 用途 |
| --- | --- | --- | --- |
| `--color-border` | `neutral-950 10%` ≈ `#0A0A0A@10%` | `neutral-50 10%` ≈ `#FAFAFA@10%` | 通用边框 |
| `--color-border-hover` | `neutral-950 20%` | `neutral-50 30%` | 边框悬停 |
| `--color-input-border` | =border | =border | 输入框边框 |
| `--color-input-border-hover` | =border-hover | =border-hover | 输入框悬停边框 |
| `--color-input-border-focused` | =`--color-brand`（sky-400 `#00BCFF`） | =`--color-brand`（sky-500 `#00A6F4`） | 输入框聚焦边框 |
| `--color-tab-border` | =border | =border | 标签页描边 |

### 1.4 强调色

| 令牌 | 亮色值 | 暗色值 | 用途 |
| --- | --- | --- | --- |
| `--color-brand` | sky-400 `#00BCFF`（换算） | sky-500 `#00A6F4`（换算） | 品牌强调/聚焦边框（styles.css:161/314） |
| `--color-accent` | sky-50 `#F0F9FF` | `sky-950 50%` ≈ `#052F4A@50%` | accent 底（styles.css:173/323） |
| `--color-hover` | neutral-200 `#E5E5E5` | =`--color-surface-hover` | 通用悬停底 |
| `--color-selected` | `neutral-950 10%` | `white 10%` | 选中底 |
| `--color-icon-blue` | =`--color-terminal-bright-blue`（亮 sky-500 `#00A6F4` / 暗 sky-600 `#0084D1`） | 同左 | 图标蓝（styles.css:162） |
| `--color-find-highlight` | `#fde68a` | `#713f12` | 查找命中底（字面） |
| `--color-find-highlight-active` | `#facc15` | `#a16207` | 当前查找命中底（字面） |

轨迹角色色（字面 hex，styles.css:163-167 / 315-319）：

| 令牌 | 亮 | 暗 | 用途 |
| --- | --- | --- | --- |
| `--color-trajectory-user` | `#2563eb` | `#60a5fa` | 用户消息 |
| `--color-trajectory-assistant` | `#0f766e` | `#2dd4bf` | 助手消息 |
| `--color-trajectory-reasoning` | `#7c3aed` | `#a78bfa` | 推理 |
| `--color-trajectory-tool-call` | `#d97706` | `#f59e0b` | 工具调用 |
| `--color-trajectory-tool-result` | `#0284c7` | `#38bdf8` | 工具结果 |

### 1.5 状态色（成功/警告/危险/信息）

ZCode 无独立 `--color-info`；「信息」语义由 sky/brand 系承担（accent、interaction-ask）。来源 styles.css:259-267 / 412-420。

| 令牌 | 亮色值 | 暗色值 | 用途 |
| --- | --- | --- | --- |
| `--color-success` | green-600 `#00A63E`（换算） | green-500 `#00C950`（换算） | 成功 |
| `--color-success-foreground` | =`--color-foreground-inverse` | 同左 | 成功前景 |
| `--color-warning` | yellow-600 `#D08700`（换算） | yellow-500 `#F0B100`（换算） | 警告 |
| `--color-warning-foreground` | =inverse | =inverse | 警告前景 |
| `--color-destructive` | red-600 `#E7000B`（换算） | red-500 `#FB2C36`（换算） | 危险 |
| `--color-destructive-foreground` | =inverse | =inverse | 危险前景 |
| `--color-idle-task` | violet-600 `#7F22FE` | violet-400 `#A684FF` | 空闲任务紫 |
| `--color-idle-task-surface` | violet-50 `#F5F3FF` | `violet-950 50%` ≈ `#2F0D68@50%` | 空闲任务底 |
| `--color-feedback-privacy-hint` | =yellow-600 `#D08700` | =yellow-300 `#FFDF20` | 隐私提示 |
| `--color-interaction-ask-surface` | sky-50 `#F0F9FF` | `sky-950 50%` | 提问等待底（styles.css:250/399） |
| `--color-interaction-ask-foreground` | sky-700 `#0069A8` | sky-300 `#74D4FF` | 提问等待文字 |
| `--color-interaction-confirmation-surface` | green-50 `#F0FDF4` | `green-950 45%` ≈ `#032E15@45%` | 确认等待底 |
| `--color-interaction-confirmation-foreground` | green-700 `#008236` | green-300 `#7BF1A8` | 确认等待文字 |
| `--color-diff-added` | green-600 `#00A63E` | green-500 `#00C950` | diff 新增 |
| `--color-diff-removed` | red-600 `#E7000B` | red-500 `#FB2C36` | diff 删除 |

Git 状态色（styles.css:272-279 / 425-432）：

| 令牌 | 亮 | 暗 |
| --- | --- | --- |
| `--color-git-modified` | amber-600 `#E17100` | amber-400 `#FFB900` |
| `--color-git-added` | teal-600 `#009689` | teal-400 `#00D5BE` |
| `--color-git-deleted` | red-600 `#E7000B` | red-400 `#FF6467` |
| `--color-git-renamed` | sky-600 `#0084D1` | sky-400 `#00BCFF` |
| `--color-git-untracked` | teal-600 `#009689` | teal-400 `#00D5BE` |
| `--color-git-ignored` | =foreground-subtlest | =foreground-subtlest |
| `--color-git-descendant` | amber-600 `#E17100` | amber-400 `#FFB900` |

### 1.6 节点/功能色（styles.css:285-305 / 438-458）

`file-node`(sky)、`skill-node`(violet)、`command-node`(slate)、`session-node`(teal)、`plugin-node`(amber) 均为「`color-mix(主色 14%, transparent)` 底 / hover 20% / 前景=主色」；dark 下底色升到 18%/24%、前景取 -300 档。subagent 节点用字面 hex：亮 `#6f7a2f`（底14%/hover20%），暗 `#b7bd75`（底18%/hover24%）。`--color-plugin-paid-plan-badge`：亮 `#ffe3c8`/文字 `#4b280f`，暗 `#5a341b`/文字 `#ffd8b8`。

### 1.7 图表色板（styles.css:221-241 / 373-390）

- `usage-chart-1..6` 亮：sky-600 `#0084D1`、teal-600 `#009689`、violet-600 `#7F22FE`、rose-500 `#FF2056`、indigo-500 `#615FFF`、cyan-600 `#0092B8`；暗：sky-500 `#00A6F4`、teal-400 `#00D5BE`、violet-400 `#A684FF`、rose-400 `#FF637E`、indigo-400 `#7C86FF`、cyan-400 `#00D3F2`。
- `context-breakdown-1..7` 亮：sky-600/500/400/300/200/100 递减 + 第 7 档 `sky-100 72% 混白`，即 `#0084D1` `#00A6F4` `#00BCFF` `#74D4FF` `#B8E6FE` `#DFF2FE` + `color-mix(oklab, sky-100 72%, white)`；暗：sky-400 及 sky-300 与 sky-200 的 84/72/60/48% 混白（styles.css:229-235、379-385）。
- `usage-heatmap-0..4`：sky-500(暗 sky-400) 0/18/24/36/42/58/62% 混 surface，末档 sky-600/sky-700/sky-300 82%/78% 混 surface（公式照抄 styles.css:237-241、386-390）。
- `workflow-rule/trace/trace-strong`：亮 `neutral-950 5.5%/30%/55%`，暗 `white 6.5%/35%/60%`。
- `animated-gradient-text-strong/soft`：亮 `rgba(10,10,10,1)`/`rgba(10,10,10,0.22)`，暗 `rgba(255,255,255,1)`/`rgba(255,255,255,0.2)`。

## 2. Zai 主题（Web/桌面默认皮肤，全部字面 hex）

来源：`.theme-zai-light`（styles.css:461-604）、`.theme-zai-dark`（styles.css:606-750）。

| 令牌 | zai-light | zai-dark |
| --- | --- | --- |
| `--color-background` | `#f8f8f8` | `#161616` |
| `--color-background-win-alt` | `#ececee` | `#2b2b2b` |
| `--color-background-alt` | `color-mix(oklab, background 70%, transparent)` | `color-mix(oklab, background-win-alt 60%, transparent)` |
| `--color-brand` | `#000000` | `#ffffff` |
| `--color-primary` / `-foreground` | `#000000` / `#ffffff` | `#ffffff` / `#000000` |
| `--color-header` / `--color-panel` | `#ffffff` | `#202020` |
| `--color-sidebar` | `#f0f0f0` | `#161616` |
| `--color-surface` / `-hover` | `rgba(13,13,13,0.03)` / `rgba(13,13,13,0.05)` | `rgba(255,255,255,0.05)` / `rgba(255,255,255,0.1)` |
| `--color-hover` / `--color-selected` | `rgba(13,13,13,0.05)` / `rgba(13,13,13,0.05)` | `rgba(255,255,255,0.05)` / `rgba(255,255,255,0.1)` |
| `--color-border` / `-hover` | `rgba(13,13,13,0.1)` / `rgba(13,13,13,0.15)` | `rgba(255,255,255,0.1)` / `rgba(255,255,255,0.15)` |
| `--color-card` / `-selected` | `#ffffff` / =input | `#2b2b2b` / =input |
| `--color-popover` / `-header` | `#ffffff` / `#f8f8f8` | `#2b2b2b` / `#202020` |
| `--color-input` / `-focused` | `#ffffff` / =input | `#2b2b2b` / =input |
| `--color-input-border-focused` | =border-hover（非 brand） | =border-hover |
| `--color-tab` / `-active` | `#f0f0f0` / `#ffffff` | `#202020` / `#161616` |
| `--color-menu` / `-hover` | `#ffffff` / `#f0f0f0` | `#2b2b2b` / `#363636` |
| `--color-secondary` | `#e6e6e6` | `#363636` |
| `--color-accent` | `#ebf4ff` | `#001d3d` |
| `--color-foreground` | =neutral-800 `#262626` | =neutral-300 `#D4D4D4` |
| `--color-foreground-subtle` / `-subtlest` | neutral-800 60% / 40% | neutral-300 60% / 30% |
| `--color-foreground-inverse` | `#ffffff` | `#000000` |
| `--color-success` / `-foreground` | `#1e8a3e` / `#ffffff` | `#46bf72` / `#000000` |
| `--color-warning` / `-foreground` | `#e07b00` / `#ffffff` | `#ff8a30` / `#000000` |
| `--color-destructive` / `-foreground` | `#e03131` / `#ffffff` | `#ff5c5c` / `#ffffff`（styles.css:708 注释：曾为黑字，因红钮固定白字而改） |
| `--color-diff-added` / `-removed` | `#1e8a3e` / `#e03131` | `#46bf72` / `#ff5c5c`（前景 zai-dark 为 `#000000`） |
| `--color-idle-task` / `-surface` | `#9e77ed` / `#f5f3ff` | `#7b5ce5` / `#160d38` |
| `--color-toast` | `#ffffff` | `#2b2b2b` |
| `--color-tooltip` / `-foreground` / `-tag` | `#f0f0f0` / `#0d0d0d` / `#e6e6e6` | `#2b2b2b` / `#f8f8f8` / `#363636` |
| `--color-tag` | `#e6e6e6` | `#363636` |
| `--color-find-highlight` / `-active` | `#fff4eb` / `#ffb26b` | `#542500` / `#ff8a30` |
| `--color-interaction-ask-surface` / `-foreground` | `#ebf4ff` / `#0066dd` | `#001d3d` / `#80beff` |
| `--color-interaction-ask-fill` | `rgba(70,191,114,0.2)` | `rgba(70,191,114,0.24)` |
| `--color-interaction-confirmation-surface` / `-foreground` | `#eaf7ee` / `#166b32` | `rgba(70,191,114,0.16)` / `#87d9a4` |
| `--color-git-modified` / `-added` / `-deleted` / `-renamed` | `#e07b00` / `#1e8a3e` / `#e03131` / `#0b7fff` | `#ff8a30` / `#46bf72` / `#ff5c5c` / `#4099ff` |
| `--color-workflow-rule` / `trace` / `-strong` | `rgba(13,13,13,0.055/0.3/0.55)` | `rgba(255,255,255,0.065/0.35/0.6)` |
| `--animated-gradient-text-strong` / `-soft` | `rgba(13,13,13,1)` / `rgba(13,13,13,0.22)` | `rgba(255,255,255,1)` / `rgba(255,255,255,0.22)` |
| `--color-usage-chart-1..6` | `#0b7fff` `#1e8a3e` `#9e77ed` `#e03131` `#e07b00` `#0aa7a7` | `#4099ff` `#46bf72` `#7b5ce5` `#ff5c5c` `#ff8a30` `#42c8c8` |
| `--color-context-breakdown-1..7` | `#0b7fff` `#338fff` `#5ca7ff` `#85bbff` `#acd0ff` `#c8ddff` `#e0ecff` | `#4099ff` `#66adff` `#80beff` `#9dceff` `#b9ddff` `#d0e8ff` `#e4f1ff` |
| `--color-file-node`（底/hover/前景） | `rgba(26,112,184,0.1/0.16)` / `#1a70b8` | `rgba(112,174,224,0.16/0.22)` / `#8fc5ef` |
| `--color-skill-node` | `rgba(116,83,176,0.1/0.16)` / `#7453b0` | `rgba(166,137,218,0.16/0.22)` / `#bda5e6` |
| `--color-command-node` | `rgba(86,98,112,0.1/0.16)` / `#566270` | `rgba(154,166,180,0.14/0.2)` / `#b5c0cc` |
| `--color-subagent-node` | `rgba(111,122,47,0.1/0.16)` / `#6f7a2f` | `rgba(183,189,117,0.16/0.22)` / `#c8cd90` |
| `--color-session-node` | `rgba(20,128,122,0.1/0.16)` / `#14807a` | `rgba(104,196,188,0.16/0.22)` / `#93d8d2` |
| `--color-plugin-node` | `rgba(184,122,26,0.1/0.16)` / `#b87a1a` | `rgba(224,174,112,0.16/0.22)` / `#efc58f` |
| 轨迹 5 色 | 与默认亮相同（`#2563eb` `#0f766e` `#7c3aed` `#d97706` `#0284c7`） | 与默认暗相同（`#60a5fa` `#2dd4bf` `#a78bfa` `#f59e0b` `#38bdf8`） |

## 3. 代码块配色

- **行内代码**：底 `--color-markdown-inline-code`（= `--color-tag`，styles.css:185，dark/zai 随 tag 变量联动）再叠 `bg-.../50`（50% 透明度），圆角 `rounded-md`(6px)，`px-1.5 py-0.5`，`font-mono text-ui-sm`（`packages/ui/src/components/ai-elements/message.tsx:1548`）。
- **围栏代码块容器**：`my-4 border border-border bg-card`（message.tsx:1567）。
- **语法高亮（shiki）**：亮 `github-light`、暗 `github-dark`（`packages/ui/src/lib/shikiHighlighter.ts:69-78`）；用户可选 vitesse/min/catppuccin 等主题（`packages/ui/src/lib/codePreviewPreferences.ts:10-19`）。默认字号 `12px`、显示行号、不换行（`packages/ui/src/lib/codePreviewSettings.ts:15-26`）。⚠️ 差异记录：根仓库 `DESIGN.md`「Code block body: default 14px」与源码默认 `fontSizePx: 12` 不一致，以源码为准（`diff-viewer.tsx:59` 默认同为 12）。
- **Diff**：`--color-diff-added`/`--color-diff-removed`（见 §1.5）；diff 渲染走 `@pierre/diffs`（Shadow DOM），背景强制 `var(--color-background)`、字体 `var(--font-mono)`（`packages/ui/src/components/ui/diff-viewer.tsx:61-68`）。
- **按钮渐变**：`.button-gradient` 亮 `linear-gradient(to bottom right, #191a1d, #747689)`，暗纯色 `#484a58`（styles.css:814-821）。
- **终端 ANSI**（xterm 主题，经 CSS 变量读取，`packages/ui/src/terminal/terminalTheme.ts:47-71` 为兜底值）：

| 令牌 | 默认亮 | 默认暗 | zai-light | zai-dark |
| --- | --- | --- | --- | --- |
| `terminal-bg` / `fg` | `#FAFAFA` / `#404040` | `#0A0A0A` / `#E5E5E5` | =background/foreground | 同左 |
| `terminal-cursor` / `cursor-accent` | `#0A0A0A` / `#FAFAFA` | `#E5E5E5` / `#0A0A0A` | `#0d0d0d` / =background | `#f8f8f8` / `#161616` |
| `terminal-selection` | `sky-500 32%` | `sky-500 26%` | `rgba(11,127,255,0.22)` | `rgba(64,153,255,0.28)` |
| `terminal-selection-inactive` | `sky-500 22%` | `neutral-50 16%` | `rgba(13,13,13,0.1)` | `rgba(255,255,255,0.1)` |
| black | neutral-700 `#404040` | neutral-800 `#262626` | `#5c5c5c` | `#363636` |
| red | red-500 `#FB2C36` | red-600 `#E7000B` | `#e03131` | `#ff5c5c` |
| green | green-500 `#00C950` | green-600 `#00A63E` | `#1e8a3e` | `#46bf72` |
| yellow | yellow-500 `#F0B100` | yellow-600 `#D08700` | `#e07b00` | `#ff8a30` |
| blue | sky-500 `#00A6F4` | sky-600 `#0084D1` | `#0b7fff` | `#4099ff` |
| magenta | fuchsia-500 `#E12AFB` | fuchsia-600 `#C800DE` | `#9e77ed` | `#7b5ce5` |
| cyan | cyan-500 `#00B8DB` | cyan-600 `#0092B8` | `#0aa7a7` | `#42c8c8` |
| white | neutral-500 `#737373` | neutral-200 `#E5E5E5` | `#adadad` | `#adadad` |
| brightBlack | neutral-500 `#737373` | neutral-500 `#737373` | `#888888` | `#747474` |
| brightRed | red-500 `#FB2C36` | red-600 `#E7000B` | `#e03131` | `#ff9999` |
| brightGreen | green-500 `#00C950` | green-600 `#00A63E` | `#1e8a3e` | `#87d9a4` |
| brightYellow | yellow-500 `#F0B100` | yellow-600 `#D08700` | `#e07b00` | `#ffb26b` |
| brightBlue | sky-500 `#00A6F4` | sky-600 `#0084D1` | `#0066dd` | `#80beff` |
| brightMagenta | fuchsia-500 `#E12AFB` | fuchsia-600 `#C800DE` | `#9e77ed` | `#a888f2` |
| brightCyan | cyan-500 `#00B8DB` | cyan-600 `#0092B8` | `#0aa7a7` | `#8ee5e5` |
| brightWhite | neutral-950 `#0A0A0A` | neutral-50 `#FAFAFA` | `#0d0d0d` | `#f8f8f8` |

（来源：styles.css:199-220 / 347-372 / 500-521 / 645-666）

- **全局滚动条**：宽/高 14px，轨道透明，thumb 圆角 `9999px`、内边框 3px transparent、最小 32px、色 `--color-border`（styles.css:757-782）。

## 4. Tailwind 4.2.2 调色板换算表（本文 hex 的出处）

ZCode 未覆盖 Tailwind 调色板，令牌引用 `--color-<family>-<step>`；OKLCH 源值取自 `tailwindcss@4.2.2` 的 `theme.css`（unpkg 校验版），hex 为 CSS Color 4 OKLab→sRGB 换算（本会话用 Node 脚本执行；越界通道按 sRGB 渲染裁剪；样本如 red-500=`#FB2C36`、green-400=`#05DF72`、violet-400=`#A684FF`、indigo-500=`#615FFF` 与 Tailwind 官方公开 hex 一致）：

| step | neutral | sky | green | red | yellow | violet | teal | fuchsia | cyan | amber | rose | indigo | slate |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 50 | `#FAFAFA` | `#F0F9FF` | `#F0FDF4` | — | — | `#F5F3FF` | — | — | — | — | — | — | — |
| 100 | `#F5F5F5` | `#DFF2FE` | `#DCFCE7` | — | — | — | — | — | — | — | — | — | — |
| 200 | `#E5E5E5` | `#B8E6FE` | `#B9F8CF` | — | — | — | — | — | — | — | — | — | — |
| 300 | `#D4D4D4` | `#74D4FF` | `#7BF1A8` | — | `#FFDF20` | `#C4B4FF` | `#46ECD5` | — | — | `#FFD230` | — | — | `#CAD5E2` |
| 400 | `#A1A1A1` | `#00BCFF` | `#05DF72` | `#FF6467` | — | `#A684FF` | `#00D5BE` | — | `#00D3F2` | `#FFB900` | `#FF637E` | `#7C86FF` | — |
| 500 | `#737373` | `#00A6F4` | `#00C950` | `#FB2C36` | `#F0B100` | — | — | `#E12AFB` | `#00B8DB` | — | `#FF2056` | `#615FFF` | `#62748E` |
| 600 | `#525252` | `#0084D1` | `#00A63E` | `#E7000B` | `#D08700` | `#7F22FE` | `#009689` | `#C800DE` | `#0092B8` | `#E17100` | — | — | `#45556C` |
| 700 | `#404040` | `#0069A8` | `#008236` | — | — | — | — | — | — | — | — | — | — |
| 800 | `#262626` | `#00598A` | — | — | — | — | — | — | — | — | — | — | — |
| 900 | `#171717` | `#024A70` | — | — | — | — | — | — | — | — | — | — | — |
| 950 | `#0A0A0A` | `#052F4A` | `#032E15` | — | — | `#2F0D68` | — | — | — | — | — | — | — |

white=`#FFFFFF`、black=`#000000`。Go 端如需其余档位，按同法从 OKLCH 换算。

## 5. 圆角档位

ZCode 未覆盖 Tailwind 圆角变量（styles.css 的 `@theme` 无 `--radius*`；`tw-theme.css:397-404`）：

| 档位 | 值 | 使用规范（根仓库 DESIGN.md「Radius」节） |
| --- | --- | --- |
| `rounded-xs` | `0.125rem` = 2px | — |
| `rounded-sm` | `0.25rem` = 4px | 最小档；嵌套最内层 |
| `rounded-md` | `0.375rem` = 6px | 菜单项、行内代码 |
| `rounded-lg` | `0.5rem` = 8px | 第一层圆角容器；按钮/输入框默认 |
| `rounded-xl` | `0.75rem` = 12px | 卡片/面板首层（DESIGN.md：首个圆角容器从 xl 起） |
| `rounded-2xl` | `1rem` = 16px | 仅白名单：主输入壳、会话状态浮板、Toast、品牌图标背板、对话框壳 |
| `rounded-3xl` | `1.5rem` = 24px | — |
| `rounded-4xl` | `2rem` = 32px | — |

嵌套递减规则：`xl → lg → md → sm`；`rounded-full`（9999px）仅限胶囊/圆。Linux 桌面窗口壳 16px、工作区面板 12px、外缩进 4px（DESIGN.md「Color Usage Rules」末条）。

## 6. 间距体系

- 基础单位 `--spacing: 0.25rem` = **4px**（Tailwind 4.2.2 默认，`tw-theme.css:325`；ZCode 未覆盖）。所有 `p-*/gap-*/m-*` = 4px 的整数倍。
- 推荐节奏（根仓库 DESIGN.md「Spacing」节）：4px 紧凑图标间距 / 8px 控件内边距 / 12px 紧凑列表行 / 16px 卡片与面板标准内边距 / 20–24px 大区块与对话框内部。
- rem 基准不随界面字号设置缩放（DESIGN.md「UI font tokens」节明确 icons/spacing/radius 用 rem 几何、不随 `--ui-font-size` 变化）。

## 7. 字号 / 行高 / 字重体系

### 7.1 UI 字号刻度（styles.css:57,143-155；默认 `--ui-font-size: 14px`）

| 令牌 | 公式 | 默认值 | 用途（DESIGN.md「Type roles」节） |
| --- | --- | --- | --- |
| `text-ui-xl` | `+4px` | 18px | Markdown h1 |
| `text-ui-lg` | `+2px` | 16px | Markdown h2 |
| `text-ui-base` | 基准 | 14px | 正文、按钮、h3–h6、工作区标题 |
| `text-ui-caption` | `-1px` | 13px | 需低正文一级的紧凑说明 |
| `text-ui-sm` | `-2px` | 12px | 次要信息、Tooltip 正文、行内代码 |
| `text-ui-xs` | `-4px` | 10px | 快捷键、徽章、极弱元数据 |
| `text-ui-2xs` | `-5px` | 9px | 仅限工作流图轴面刻度（受限例外） |
| `text-mobile-input-safe` | 固定 | 16px | 移动 Web 输入防 iOS 聚焦缩放 |

### 7.2 行高

- `text-ui-*` 未绑定配对行高（styles.css:143-155 只有尺寸变量），行高继承或按需用 `text-ui-base/relaxed` 等指定（message.tsx 等大量使用 `/relaxed`）。
- Tailwind 默认档（`tw-theme.css:347-356`，如需对照）：`text-xs`=12px/1.333、`text-sm`=14px/1.4286、`text-base`=16px/1.5、`text-lg`=18px/1.5556、`text-xl`=20px/1.4。`line-height: relaxed`=1.625、`snug`=1.375（Tailwind 默认）。

### 7.3 字重与字体

- 字重（`tw-theme.css:374-382`）：`font-normal` 400 / `font-medium` 500 / `font-semibold` 600 / `font-bold` 700。DESIGN.md 规则：标题标签偏好 `font-medium`，避免过重；h3–h4 semibold、h5 medium、h6 normal。
- `font-sans` 未覆盖 → Tailwind 默认：`ui-sans-serif, system-ui, sans-serif, 'Apple Color Emoji', 'Segoe UI Emoji', 'Segoe UI Symbol', 'Noto Color Emoji'`。
- `font-mono` 覆盖（styles.css:140-142）：`ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", "Microsoft YaHei UI", "Microsoft YaHei", "PingFang SC", "Noto Sans CJK SC", monospace`（在等宽栈中插入 CJK，避免中文落宋体）。
- 大写等宽微标签字距 `--tracking-wf-label: 0.09em`（styles.css:153）。

## 8. 阴影

未覆盖 Tailwind 默认（`tw-theme.css:406-412`）：

| 档位 | 值 |
| --- | --- |
| `shadow-2xs` | `0 1px rgb(0 0 0 / 0.05)` |
| `shadow-xs` | `0 1px 2px 0 rgb(0 0 0 / 0.05)` |
| `shadow-sm` | `0 1px 3px 0 rgb(0 0 0 / 0.1), 0 1px 2px -1px rgb(0 0 0 / 0.1)` |
| `shadow-md` | `0 4px 6px -1px rgb(0 0 0 / 0.1), 0 2px 4px -2px rgb(0 0 0 / 0.1)` |
| `shadow-lg` | `0 10px 15px -3px rgb(0 0 0 / 0.1), 0 4px 6px -4px rgb(0 0 0 / 0.1)` |
| `shadow-xl` | `0 20px 25px -5px rgb(0 0 0 / 0.1), 0 8px 10px -6px rgb(0 0 0 / 0.1)` |
| `shadow-2xl` | `0 25px 50px -12px rgb(0 0 0 / 0.25)` |

特色阴影均为内嵌描边式，如 `.wf-more-face` 的 `0 0 0 1.5px var(--color-background)`（styles.css:1443）、运行灯 `0 0 0 3px warning 20%`（styles.css:1566）。

## 9. 动效时长（源码字面值，均在 styles.css）

| 动效 | 时长/缓动 | 行号 |
| --- | --- | --- |
| 渐变流光 `gradient-flow` | 4s linear infinite | 838 |
| CUA 组渐变 | 1s linear + 0.5s delay | 862-863 |
| 流式文本入场 | 900ms `cubic-bezier(0.16,1,0.3,1)` | 974 |
| 草稿瀑布入场 | 260ms `cubic-bezier(0.22,1,0.36,1)` | 925 |
| 折叠收起 | 300ms ease-in-out | 994-998 |
| 点赞回弹/粒子 | 450ms `cubic-bezier(0.34,1.56,0.64,1)` / 560ms +60ms delay | 1040,1055 |
| 闹钟摆动 | 600ms ease-in-out infinite | 1059 |
| 图片加载 shimmer | 1.8s linear | 954 |
| 倒计时条 | 线性，时长=剩余毫秒 | 1149 |
| 远程连接呼吸 | 1.8s ease-in-out infinite | 1161 |
| 更新扫光 | 1.15s `cubic-bezier(0.65,0,0.35,1)` | 1182 |
| Browser Use 呼吸 / 变暗过渡 | 900ms ease-in-out / 160ms ease-out | 1229,1238 |
| 搜索命中/分支高亮 | 1.2s ease-out / 1.6s ease-out | 1277,1296 |
| 工作流速度体系 | fast 120ms / base 160ms / enter 200ms / ink 320ms，缓动 `cubic-bezier(0.22,0.61,0.36,1)`，心跳 1.6s | 1357-1365 |
| 工作流杂项 | draw 420ms、mark 180ms、caret 1s step-end、ws-land 1.4s、landed 1.2s、shine 2.4s | 1605-1731 |
| 表情脸 | 眨眼 230ms、变形 180ms、跳 240ms、浮 600ms、抖 90ms、视线 360ms ease-in-out | 1768-1913 |

`prefers-reduced-motion: reduce` 时以上全部归零（styles.css:929-933、1021-1030、1119-1124 等）。组件进出常配 tw-animate-css 的 `animate-in/out`（时长用 Tailwind 默认 150ms）。

## 10. 来源与许可

- 来源仓库：`/Users/chengliang/code-repositories/ZCode`（检出 commit `872ad96` "feat: open source"）。
- 许可证：**Apache-2.0**（`LICENSE`、`package.json:5`）。本仓库（KeenCode）AGENTS.md 已获用户授权以 ZCode 为桌面/Web 界面视觉基线复用其 UI 组件与 CSS；本文数值提取仅用于该授权范围内的 Go 原生 UI 还原。
- 主要来源文件：
  - `packages/ui/src/styles.css`（全部主题变量、滚动条、动效；2053 行）
  - `packages/ui/src/useTheme.ts`、`packages/web/src/webThemeSeed.ts`、`packages/web/src/main.tsx`（主题初始化与持久化）
  - `packages/ui/src/lib/shikiHighlighter.ts`、`codePreviewSettings.ts`、`codePreviewPreferences.ts`、`components/ai-elements/message.tsx`（代码块）
  - `packages/ui/src/terminal/terminalTheme.ts`（xterm 兜底）
  - 根 `DESIGN.md`（字号角色、间距节奏、圆角层级规范）
  - `tailwindcss@4.2.2` `theme.css`（调色板 OKLCH 与默认 radius/spacing/shadow/字重；MIT 许可，hex 为本次会话换算）
- 未覆盖/待验证：`--color-muted-*` 等个别 shadcn 包装类（input-group.tsx:26）在本仓库无定义值；shiki 各主题的完整语法色板随 shiki 包分发（本文只记录主题名与默认值），Go 端如需逐 token 语法色需另行提取 shiki 主题 JSON。

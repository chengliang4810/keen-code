# Codex 桌面端设计系统（DESIGN.md）

> 来源：反编译 `/Applications/ChatGPT.app`（Codex 桌面端内嵌其中，Electron 架构）的 `app.asar`。
> 方法：解析渲染层 CSS（核心 `app-shared-11c21cbb0024.css` 869KB，共 241 个 CSS chunk）与主进程 `main-DUHZj4_w.js`，逐条提取真实令牌值。全部数值为解包实测，推断处单独标注。
> 提取日期：2026-09-19。解包产物：`/tmp/codex-design/app`。
> 配套分析：`docs/research/2026-09-19-codex-desktop-design-analysis.md`

---

## 1. 架构与主题机制

| 项 | 值 |
|---|---|
| 应用形态 | Electron + Vite + React + Tailwind v4 |
| 主题机制 | `--lightningcss-light(值)` / `--lightningcss-dark(值)` 成对函数变量，同一令牌同时携带双主题值，由 `data-theme` 或 `prefers-color-scheme` 切换 |
| 窗口类型 | `data-codex-window-type=electron \| browser \| chrome-extension`，electron 桌面窗口有专属字号/字重覆盖 |
| 窗口缩放 | `--codex-window-zoom: 1 / 1.25 / 1.5`（缩放联动圆角、行高等） |
| 主题变体 | `--theme-variant: light \| dark`（容器查询 `@container style(--theme-variant:light)` 用于局部主题分支，如代码注释斜体） |
| CSS 变量总量 | 1906 个（app-shared + app-initial + app-primary） |

---

## 2. 字体系统

### 2.1 字体族

| 令牌 | 生效值 | 说明 |
|---|---|---|
| `--font-sans-default` | `-apple-system-body, ui-sans-serif, -apple-system, system-ui, "Segoe UI", "Helvetica", "Apple Color Emoji", "Arial", sans-serif, "Segoe UI Emoji", "Segoe UI Symbol"` | macOS = SF Pro；Windows 落到 Segoe UI。无按 OS 的字体分支，靠栈序 fallback |
| `--font-mono-default` | `ui-monospace, "SFMono-Regular", "SF Mono", menlo, consolas, "Liberation Mono", monospace` | macOS = SF Mono |
| `--font-serif` | `ui-serif, Georgia, Cambria, "Times New Roman", Times, serif` | |
| `--font-openai-sans` | `"OpenAI Sans", var(--font-sans-default)` | 打包 Regular/Medium/Semibold 三个 woff2（400/500/600，`font-display: swap`），仅 `.font-openai-sans` 品牌位使用，聊天与侧边栏不用 |
| `--font-sans` | `var(--font-ui-family, var(--font-sans-default))` | UI 主字体 |
| `--font-ui-family` | 默认 `var(--font-sans-default)`；嵌入场景 `inherit` / `var(--vscode-font-family)` | |
| `--font-content` | `var(--codex-content-font-family, var(--font-sans))` | 对话内容字体 |
| `--font-mono` | `var(--font-code-family, var(--font-mono-default))`；嵌入场景跟随 `--vscode-editor-font-family` | |
| Tailwind preflight | `-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", "Noto Sans", Arial, sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol", "Noto Color Emoji"` | `--default-font-family` 兜底层 |

### 2.2 字号阶梯（electron 窗口覆盖后的实际值）

| 令牌 | 值 | 行高 | 用途 |
|---|---|---|---|
| `--text-3xs` | 9px（.5625rem） | 9px | 极小标签 |
| `--text-2xs` | 10px（.625rem） | 12px | |
| `--text-xs` | **12px** | 16px | 辅助小字 |
| `--text-sm` | **13px** | 20px | 次级 UI 文本、菜单、tooltip、badge |
| `--text-base` | **14px** | **21px（1.5）** | 主字号 |
| `--text-md` | 16px | 26px | 控件 lg 档 |
| `--text-lg` | 16px | 28px | |
| `--text-xl` | 28px | 32px | 大标题 |
| `--text-2xl` | 36px | 48px | |
| `--text-3xl` | 48px | 54px | |
| `--text-4xl` | 72px | 80px | |
| `--text-document` | 14px | — | 文档视图 |

- 聊天正文 `--codex-chat-font-size` = `var(--font-ui-size)` = **14px**（fallback 13px）。
- 代码字号 `--codex-chat-code-font-size` = `var(--font-code-size)` = **13px**（fallback 12px）；diff 字号 = 代码字号 − 1px。
- DIL 标题体系（对话渲染器）：sm 16px/26px、md 18px/28px、lg 20px/28px、xl 24px/32px、2xl 36px/40px、3xl 48px/48px、4xl 60px/60px、5xl 72px/72px。

### 2.3 字重与行高

| 令牌 | 值 |
|---|---|
| `--font-ui-weight` | **430**（electron 窗口覆盖；默认 400）— SF Pro 可变字体非整数中间值，正文观感核心 |
| `--font-weight-light/normal/medium/semibold/bold` | 300 / 400 / 500 / 600 / 700 |
| strong 字重规则 | text-3xs~sm 用 medium(500)；text-md/lg 用 semibold(600) |
| `--leading-tight / snug / normal / relaxed / dense` | 1.25 / 1.375 / 1.5 / 1.625 / calc(7/6) |
| 正文渲染 | `body { font-family: var(--font-ui-family); font-weight: var(--font-ui-weight); }` |

---

## 3. 色彩系统

### 3.1 灰阶（全部 25 阶，light / dark）

| 阶 | Light | Dark | | 阶 | Light | Dark |
|---|---|---|---|---|---|---|
| gray-0 | #ffffff | #0d0d0d | | gray-500 | #5d5d5d（双主题固定） | |
| gray-25 | #fcfcfc | #101010 | | gray-550 | #4f4f4f | #767676 |
| gray-50 | #f9f9f9 | #131313 | | gray-600 | #414141 | #8f8f8f |
| gray-75 | #f3f3f3 | #161616 | | gray-650 | #393939 | #9f9f9f |
| gray-100 | #ededed | #181818 | | gray-700 | #303030 | #afafaf |
| gray-150 | #dfdfdf | #1c1c1c | | gray-750 | **#282828** | #b9b9b9 |
| gray-200 | #cdcdcd | #212121 | | gray-800 | #212121 | #cdcdcd |
| gray-250 | #b9b9b9 | #282828 | | gray-850 | #1c1c1c | #dcdcdc |
| gray-300 | #afafaf | #303030 | | gray-900 | #181818 | #ededed |
| gray-350 | #9f9f9f | #393939 | | gray-925 | #161616 | #f3f3f3 |
| gray-400 | #8f8f8f | #414141 | | gray-950 | #131313 | #f3f3f3 |
| gray-450 | #767676 | #4f4f4f | | gray-975/1000 | #101010 / #0d0d0d | #f9f9f9 / #ffffff |

固定灰（不随主题）：`--gray-fixed-0 #fff`、`-150`、`-300 #afafaf`、`-400`、`-500 #5d5d5d`、`-700 #303030`。

### 3.2 品牌色系（每系 20 阶，节选关键阶；light 值）

| 色系 | 50 | 100 | 300 | 500 | 700 | 900 |
|---|---|---|---|---|---|---|
| blue | #e5f3ff | #99ceff | #339cff | #0169cc | #003f7a | #00284d |
| green | #d9f4e4 | #8cdfad | #40c977 | #00a240 | #00692a | #003716 |
| red | #ffd9d9 | #ffa4a2 | #ff6764 | #e02e2a | #911e1b | #4d100e |
| yellow | #fff6d9 | #ffe48c | #ffd240 | #e0ac00 | #916f00 | #4d3b00 |
| purple | #efe5fe | #ceb0fb | #ad7bf9 | #8046d9 | #532d8d | #2c184a |
| pink | #ffe8f3 | #ffbada | #ff8cc1 | #e04c91 | #963c67 | #4d1f34 |
| orange | #ffe7d9 | #ffb790 | #ff8549 | #e25507 | #923b0f | #4a2206 |

dark 模式整体上移 1–2 阶取色（如 git added 用 green-300）。

### 3.3 语义层（核心，已按双重解析修正为最终渲染值）

> 修正说明：早期版本此表的 Dark 列直接抄了引用令牌的 light 侧 hex（如 gray-750 #b9b9b9），是错的。多数语义令牌在 dark 主题下切换引用了另一个调色板令牌（如 `--color-text` dark 侧 = `--gray-850`），需按 3.6 节规则二次解析。

| 令牌 | Light 最终值 | Dark 最终值 |
|---|---|---|
| `--color-text`（正文） | **#282828**（gray-750） | **#dcdcdc**（gray-850 dark 侧） |
| `--color-text-secondary` | **#5d5d5d**（gray-500，固定灰） | **#afafaf**（gray-700 dark 侧） |
| `--color-text-tertiary` | **#8f8f8f**（gray-400） | **#8f8f8f**（gray-600 dark 侧，恰好同值） |
| `--color-text-info` | blue-500 #0169cc | blue-200 dark 侧 #66b5ff |
| `--color-text-danger` | red-700 #911e1b | red-500 #e02e2a |
| `--color-surface` | gray-0 **#ffffff** | gray-200 dark 侧 **#212121** |
| `--color-surface-elevated` | #ffffff | #303030 |
| `--color-border` | 基色 #0d0d0d @ 10% 透明 | 基色 #ffffff @ 12% 透明（透明度也变） |
| `--color-border-strong` | #0d0d0d @ 15% | #ffffff @ 20% |
| `--color-background-user-message` | #0d0d0d @ 5% 透明 | #ffffff @ 8% 透明 |
| `--color-codex-git-added` | green-500 #00a240 | green-300 #40c977 |
| `--color-codex-git-deleted` | red-600 #911e1b | red-400 #ff6764 |
| 窗口底色（原生） | #ffffff | #202020 |
| modal backdrop | #0000004d | #00000080 |

### 3.4 Alpha 体系

- `--alpha-base`：light `#0d0d0d` / dark `#fff`。
- 阶梯：`--alpha-01/02/04/05/06/08/10/12/15/16/20/25/30/35/40/50/60/70` = base 的对应百分比透明（`color-mix(in oklab, base n%, transparent)`）。边框、ghost hover、遮罩全部由 alpha 系派生，不写死灰色。

### 3.5 代码语法高亮色板（--color-codex-syntax-*，light → dark）

| 令牌 | Light | Dark |
|---|---|---|
| `name`（标识符） | #1f4e94 | #63a8f8 |
| `keyword`（关键字） | #ab4f7a | #f8a6c8 |
| `string`（字符串） | #3a843f | #83d197 |
| `literal`（常量/数字） | #ac4f23 | #f1a275 |
| `attribute`（属性） | #b8802b | #f9dc78 |
| `variable`（变量） | #643cae | #b897f4 |
| `comment`（注释） | gray-fixed-550 **#4f4f4f**（双主题固定） | 同左 |
| `error`（错误） | red-600 #911e1b | dark 红变体 |

- 规律：dark 模式整体提亮至同色相的高明度变体；注释用固定灰不受主题影响；light 下注释斜体（容器查询 `--theme-variant:light` 控制）。

### 3.6 主题切换颜色变化对照（light ⇄ dark 实测）

> 背景：运行时通过根元素 `data-theme="dark"/"light"` 属性切换（配合 `prefers-color-scheme` 跟随系统），不修改磁盘 CSS。切主题时变化的令牌集合双向对称，下表不分切换方向。

**机制与规模**：

- 全部 `--lightningcss-light/dark` 双值令牌共 **227 个**；切主题时定义翻转的 **226 个**；唯一双值相同的是焦点环 `--app-color-border-focus = blue-300 #339cff`（两种主题恒定）。
- **双重解析规则**：令牌的 light/dark 侧引用的是"另一个双值令牌"时需二次解析。例：`--color-text` = light 侧 `--gray-750`（→#282828）/ dark 侧 `--gray-850`（dark 解析 →#dcdcdc）；并非"同一令牌取 dark 支"。
- 226 个翻转令牌中有 **13 个定义翻转但最终渲染色相同**（视觉不变）：`--color-text-tertiary`（#8f8f8f），以及 info/warning/caution/danger/success/discovery 六色的 surface 背景与 border 共 12 个（#0285ff/#fb6a22/#ffc300/#fa423e/#04b84c/#924ff7，两侧同值）。

**核心令牌最终渲染值对照**（完整 226 项见 `/tmp/codex-design/theme-resolved.json`）：

| 令牌 | Light | Dark |
|---|---|---|
| `--color-text` | #282828 | #dcdcdc |
| `--color-text-secondary` | #5d5d5d | #afafaf |
| `--color-surface` | #ffffff | #212121 |
| `--color-surface-secondary` | #f9f9f9 | #181818 |
| `--color-surface-elevated` | #ffffff | #303030 |
| `--color-surface-elevated-secondary` | #f9f9f9 | #414141 |
| `--app-color-text-foreground` | #1a1c1f | #dfdfdf |
| `--app-color-background-surface` | #ffffff | #181818 |
| `--app-color-background-surface-under` | #f9f9f9 | black |
| `--color-background-primary-soft` | #ededed | #303030 |
| `--color-background-primary-solid` | #181818 | #f3f3f3 |
| `--color-ring`（焦点环） | #0169cc | #0285ff |
| `--color-text-info` | #0169cc | #66b5ff |
| `--color-text-danger` | #911e1b | #e02e2a |
| `--menu-item-background-color` | 基色 8% 透明 | 基色 10% 透明（基色黑⇄白） |
| `--modal-backdrop-background` | #0000004d | #00000080 |
| `--alpha-base`（透明色基） | #0d0d0d | #ffffff |

**原生层同步变化**：窗口 `backgroundColor` #ffffff ⇄ #202020；titleBarOverlay/红绿灯 `symbolColor` #1f1f1f ⇄ #ffffff（由 `nativeTheme.shouldUseDarkColors` 驱动）。

**分类统计**（226 个翻转令牌）：背景/表面色 95、组件令牌 52、文字/图标色 37、原始色板 24（gray 全阶梯对称反转：light 第 N 阶 = dark 第 1000−N 阶）、边框/分隔线 18。

---

## 4. 圆角系统

- 全局缩放：`--codex-corner-radius-scale: 1.25`（另一主题为 1）。语义圆角 = 基础值 × 1.25。
- **超椭圆圆角**：`--codex-corner-shape: superellipse(1.5)`（CSS corner-shape）；composer 用 `superellipse(1.1)`。

| 令牌 | 基础值 | ×1.25 后 | 使用处 |
|---|---|---|---|
| `--radius-xs` | 2px / 4px | 2.5 / 5px | badge-sm |
| `--radius-sm` | 4px / 6px | 5 / 7.5px | badge、tooltip 输入 |
| `--radius-md` | 6px / 8px | 7.5 / 10px | tooltip、icon-button |
| `--radius-lg` | 8px / 10px | 10 / 12.5px | sidebar-item、turn 按钮、diff 文件行 |
| `--radius-xl` | 12px | 15px | button、modal |
| `--radius-2xl` | 16px | 20px | menu |
| `--radius-3xl` | 20px / 24px | 25 / 30px | |
| `--radius-4xl` | 24px / 32px | 30 / 40px | composer 备选 |
| `--radius-full` | 9999px | — | 会话行、avatar |
| composer 实际 | `calc(spacing*7)` | **28px** | 输入框（备选 22px/`radius-4xl`） |

---

## 5. 间距与尺寸

- 基数 `--spacing: .25rem`（4px），组件一律 `calc(var(--spacing) * n)`。

### 5.1 核心布局尺寸

| 令牌 | 值 | 说明 |
|---|---|---|
| `--height-toolbar` | 46px（备选 52px） | 主工具栏；sm 36px、pane 40px |
| `--height-token-nav-row` | `text-base*1.5 + padding-row-y*2` = **29px**（30/36px 覆盖） | 会话列表行高 |
| `--height-token-mode-switch` | 32px | 模式切换 |
| `--height-token-settings-row` | 64px | 设置行 |
| `--conversation-item-gap` | 16px | 消息块间距 |
| `--conversation-grouped-item-gap` | 4px | 同组消息间距 |
| `--thread-content-max-width` | 42rem / 48rem（compact 40rem） | 对话正文最大宽 |
| `--thread-gutter` | 16px | 对话区横向留白 |
| `--thread-content-top-inset` | 32px / 46px / 0 | 顶部内缩 |
| `--thread-floating-content-*-inset` | 16 / 20 / 12px | 浮动内容上下内缩 |
| `--padding-panel-base` | 20px / 12px | 面板内边距 |
| `--padding-row-y / -x` | 4–5px / 8px | 列表行内边距 |
| `--min-height-composer` | 44px | 输入框最小高 |
| `--line-height-composer` | 20px | 输入行高 |
| `--composer-expanded-height` | `min(100svh/zoom − 46px − 128px, 100cqh − 64px, 48rem)` | 展开最大高 |
| `--user-chat-width` | `min(456px, 100%)`（备选 70%） | 用户气泡最大宽 |
| 容器断点 | `--container-3xs 16rem ~ 5xl 64rem` | 容器查询 |
| 毛玻璃阶梯 | `--blur-xs/sm/md/lg/xl/2xl/3xl` = 4 / 8 / 12 / 16 / 24 / 40 / 64px | composer 底部表面用 `blur-lg` 16px |

---

## 6. 阴影与描边

### 6.1 Elevation（组件级）

| 令牌 | 值 |
|---|---|
| `--elevation-stroke` | `0 0 0 .5px var(--color-border-strong)` |
| `--elevation-prominent` | stroke + `0 3px 7.5px #0000000a` + `0 0 20px #0000000d` |
| `--elevation-sidebar` | stroke + `0 3px 7.5px #00000008` + `0 0 16px #00000005` |
| `--elevation-composer` | `0 0 0 1px #0000000a, 0 2px 8px #0000000a, 0 4px 80px 8px #00000006` |
| `--elevation-composer-dark` | 上式 + `inset 0 0 1px 0 #ffffff33` |

### 6.2 Shadow 阶梯（几何 × alpha 组合式）

- 结构：`--shadow-N = var(--elevation-N-geo) rgb(var(--shadow-color) / var(--shadow-alpha-N))`；`-strong` ×1.25、`-stronger` ×1.6。
- alpha：100/200 级 0.08（dark 0.2）、300 级 0.1（dark 0.36）、400 级 0.12（dark 0.3）。
- 固定值：sm `0 1px 2px -1px #00000014`、md `0 2px 4px -1px #00000014`、lg `0 4px 8px -2px #0000001a`、xl `0 8px 16px -4px #0000001f`、2xl `0 16px 32px -8px #00000030`、card `0 4px 16px #0000000d`。
- 发丝线：`--shadow-hairline = 0 0 0 .5px #0000001a`（宽 .5px/1px，色 #00000014/#ffffff1a）。
- tooltip：`0 8px 18px #0f172a33`。

---

## 7. 动效系统

| 项 | 值 |
|---|---|
| 默认时长 | `--default-transition-duration: .15s` |
| 默认缓动 | `--default-transition-timing-function: cubic-bezier(.4, 0, .2, 1)` |
| 实际时长分布 | .15s 为主，其次 .2s / .3s / .5s；`--transition-duration-relaxed`（composer rail 等） |
| 缓动分布 | ease ×90、linear、ease-out、`cubic-bezier(.4,0,.2,1)`、steps(48/120,end) |
| modal 进出 | 进 .6s / 出 .3s；出场上移 `--modal-out-y: 20px` |
| 启动画面 | logo 56px；180ms ease-out 淡入 + 2200ms `cubic-bezier(0.4,0,0.2,1)` shimmer 循环；支持 `prefers-reduced-motion` |
| keyframes | 约 150 个：shimmer/pulse/rotate/scale-in/fade-in 系统件；`typing-dot-wave`（打字点）；`curtainRaise/Lower`；`marqueeTextScroll`；`word-arrival`/`_d-word-enter`（逐词入场）；`loading-indeterminate`；`composerOverlayEnter`；`shake`（错误晃动）；`edge-fade`（边缘渐隐）等 |

### 7.1 典型动画实测参数

| 动画 | 定义 | 时序 |
|---|---|---|
| 打字指示器 `typing-dot-wave` | translateY: 0 → +1.2px(25%) → −2px(55%) → 0(70%) | 1s ease-in-out 无限；第 2/3 点延迟 +0.1s/+0.2s |
| `pulse` | 50% 处 opacity .5 | — |
| `token-pulsing-dot` | scale 1 → 1.25(50%) → 1 | 状态点呼吸 |
| shimmer 家族 | 背景位移动画（如启动画面 140% → −105%） | 2.2s 循环 cubic-bezier(.4,0,.2,1) |

---

## 8. 控件规格

### 8.1 按钮（--button-*）

- 字号三档：xs 12 / sm 13 / base 14px；字重 400。
- 图标-文字 gap：sm 3px / md 4px / lg 6px；图标偏移 −1px / −2px。
- 圆角 `--radius-xl`（15px）；焦点环 `--ring` 系，offset −1px。
- 高度用 control 尺寸阶梯。

### 8.2 Control 尺寸阶梯（按钮/输入通用）

| 档 | 高度 | 图标 | gutter |
|---|---|---|---|
| 4xs | 20px | — | 10px |
| 3xs | 22px | 14px | — |
| 2xs | 24px | — | 6px |
| xs | 26px | 14px | 8px |
| sm | 28px | 16px | 10px |
| md | 32px | 18px | 12px |
| lg | 36px | 20px | 14px |
| xl | 40px | 22px | 16px |
| 2xl | 44px | 24px | — |
| 3xl | 48px | — | — |

- 胶囊形态 gutter 缩放 ×1.33（`--control-gutter-pill-scaling`）。

### 8.3 开关（switch）

- 轨道 32×19px；thumb = 轨高 − 2×3px offset；thumb 阴影 `0 1px 2px #0003`。
- 轨道底：gray-150 / dark gray-400；hover gray-200 / gray-450；选中 `--color-blue`。
- disabled：轨道 gray-100 / gray-300。

### 8.4 单选（radio）

- indicator 直径 = `--font-text-md-size`（16px）；孔 6px；选中孔色 = 正文色。
- 组内列距 10px、行距 20px、项内 gap 6px。

### 8.5 菜单（menu）

- 字号 sm 13px、行高 text-sm；圆角 `radius-2xl` 20px。
- item padding `8px 12px`、项间 gap 6px、gutter 6px。
- hover 背景：alpha-08（light）/ alpha-10（dark）；分隔线 `--color-border`。

### 8.6 Modal

- 圆角 `radius-xl` 15px；内边距 20px；backdrop 见 3.3。
- 进 .6s / 出 .3s；fade 变体带 `blur(1px)` 背板 + `0 8px 16px #00000012` 阴影 + 1px 描边。

### 8.7 Tooltip

- 字号 sm 13px、行高 1.45、字重 400；圆角 `radius-md` 7.5–10px。
- padding：sm `8px 12px`、md `12px 16px`、lg `14px 18px`。
- compact 变体：gray-700 底、gray-0 字、hover gray-600，padding `2px 8px`。

### 8.8 Badge / Avatar

- Badge：字号 xs/sm、semibold、圆角 xs/sm、高 20/22/24px、tracking sm 档加宽。
- Avatar：28px、全圆角、描边 alpha-04/15、组内叠放 −8px、遮罩挖边 3px、溢出数字缩放 0.3–0.45。

### 8.9 焦点环（--color-ring-*）

- 基准：`--color-ring` = blue-500 #0169cc（light）/ blue-400（dark）。
- danger 系：red-200 #ffc4c4；info/caution/discovery 各系 soft/outline/ghost/solid 四变体同值（直接引用系基准色）。
- 按钮：`--button-ring-offset: -1px`（环内缩 1px 紧贴圆角）；焦点环叠加在 `radius-xl` 圆角上。

---

## 9. 界面组件规格

### 9.1 工具栏 / 侧边栏 / 会话列表

- 工具栏 46px，红绿灯在其内垂直居中 (16,16)。
- 侧边栏：背景透明 + 原生 vibrancy `menu`；`.sidebar-item` 圆角 `radius-lg` 12.5px + superellipse(1.5)；icon-button 24px/`radius-md`/padding 4px。
- 会话行：高 29px（browser 36px）、全圆角胶囊、行内 padding-block 4px。
- 列表滚动渐隐 mask：底部 `#000000e0 → #00000085 → #0000002e → transparent`（6/3/1 个 spacing 处过渡）。

### 9.2 对话区 / 用户气泡

- 正文容器 max-width 42/48rem；消息块间 16px、同组 4px。
- 用户气泡类名：`bg-user-message text-user-message min-w-0 max-w-(--user-chat-width) overflow-hidden break-words px-(--thread-content-margin) py-2.5 rounded-2xl leading-none`。
- 即：圆角 20px、纵向 10px、背景 = 正文色 5% 透明、最大宽 min(456px,100%)。

### 9.3 Composer（输入框）

- 圆角 28px + `superellipse(1.1)`；背景 `--color-surface-elevated`；阴影 `--elevation-composer`（dark 加 inset 高光）。
- 行高 20px、最小高 44px、gutter 12px；输入 padding `0 12px`；placeholder 透明度 0.5。
- 附件区圆角 = composer 圆角 − inset(8px) − 4px；内嵌 8px。
- 底部表面：`color-mix(in oklab, --color-background-primary-soft 90%, transparent)` + `blur(--blur-lg)`。
- 建议条内缩 = 工具栏 padding 16px + home 内缩 13px。

### 9.4 代码块 / Diff

- 代码注释色 `--color-codex-syntax-comment`（light 下斜体）；InlineCodePane 最大高 56.25cqw；查看行高 `max(1.5em, 20px)`。
- Diff：字体 `--font-mono`、字号 = 聊天代码字号 −1px、行高 ×1.8（≈21.6px）、行号列最小 4ch；added `--color-codex-git-added`、deleted `--color-codex-git-deleted`；surface `--color-surface`（可覆盖为 elevated-secondary 50% 混合）；文件行底部 `radius-lg`；header padding 沿用 turn 资源卡（12/16px 横向）。
- turn 操作按钮：高 32px、宽 32px、`radius-lg`；hover 用 `color-mix(primary-ghost-hover 30%, transparent)`。

### 9.5 滚动条

- 默认整窗隐藏：`::-webkit-scrollbar { display:none }` + `scrollbar-width: none`（图表等区域 width:0）。
- overlay 场景：`scrollbar-color: var(--color-border) transparent` 或 `--alpha-30`，另有 `scrollbar-width: thin` 与 `scrollbar-color: transparent !important` 的强制隐藏态。

---

## 10. 原生窗口层（主进程实测）

| 窗口 | macOS | Win/Linux |
|---|---|---|
| 主窗口 primary | `titleBarStyle: hiddenInset` + `trafficLightPosition {x:16, y: round((46*zoom−14)/2)}`；`acceptFirstMouse`；fallback `vibrancy: 'menu'` | `titleBarStyle: hidden` + `titleBarOverlay {height:36px, color:#00000000, symbolColor:#1f1f1f/#ffffff}` |
| secondary | `vibrancy:'menu'` + `titleBarStyle:'default'` | default |
| hud/快捷 | hiddenInset + (10,10)，不可最小化/最大化/全屏，置顶 | 同 overlay 机制 |
| quickChat | 透明窗口 `backgroundColor:#00000000`、hasShadow | — |
| 窗口底色 | dark `#202020` / light `#ffffff` | 同 |
| 主题同步 | `nativeTheme.shouldUseDarkColors` 驱动 symbolColor | — |

---

## 11. 图标与层级

- 图标基准：`--icon-leading-size` 16px（20px 档 = `spacing*5`）；secondary 16px；primary-action 20px；disclosure 12px；侧边栏图标按钮 24px。
- z-index 实际分布：1/2/3/5/10/42/50/51/55/56/60/80；浮层上限 `--max-app-overlay-z-index: 2147480000`。

---

## 12. 附录：变量体系与提取方法

- 变量前缀统计（去重 1906 个）：`--color-*` 766、`--tw-*` 94、`--font-*` 89、`--app-*` 80、`--gray-*` 50、`--text-*` 43、`--control-*` 30、`--shadow-*` 29、`--composer-*` 27、`--radius-*` 24、`--badge-*` 24、`--button-*` 20、`--green/red/pink/orange/yellow/purple/blue-*` 各 20、`--alpha-*` 20、`--input-*` 18、`--tooltip-*` 17、`--spacing-*` 16、`--thread-*` 13、`--switch-*` 13、`--codex-*` 13、`--menu-*` 12、`--modal-*` 12、`--padding-*` 11、`--radio-*` 11、`--icon-*` 10、`--height-*` 10、`--avatar-*` 10、`--diffs-*` 10、`--sidebar-*` 9、`--elevation-*` 9 等。
- 核心来源文件：`webview/assets/app-shared-11c21cbb0024.css`（:root 令牌群）、`app-initial-19d25b9d212e.css`、`app-primary-d77f37ba49ff.css`、`response-7c6f40e38d30.css`（DIL 排版）、`chatgpt-code-block-highlighting-a44c30b43d60.css`、`user-message-*.js`（气泡类名）、`.vite/build/main-DUHZj4_w.js`（窗口配置）。
- 完整变量表导出：`/tmp/codex-design/all-vars.txt`（临时产物，重启后失效；如需长期保留请复制入库）。
- 平台差异结论：无 Windows 专属字体分支；字号/字重覆盖按 `data-codex-window-type` 划分。Windows 11 上 `ui-sans-serif` = Segoe UI Variable（430 可插值），Windows 10 固定字重会舍入（推断，已标注）。

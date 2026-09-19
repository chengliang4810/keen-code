# Codex 桌面端设计分析（反编译报告）

- 日期：2026-09-19
- 分析对象：`/Applications/ChatGPT.app`（Codex 桌面端内嵌于 ChatGPT.app，Electron 架构）
- 版本快照：应用 2026-09-19 更新
- 方法：解包 `Contents/Resources/app.asar`（342MB）→ 渲染层 `webview/`（Vite + React，241 个 CSS chunk，核心 `app-shared-11c21cbb0024.css` 869KB）+ 主进程 `.vite/build/main-DUHZj4_w.js`。逐条提取 CSS 自定义属性与类名，非截图猜测。

## 1. 技术形态

- Electron 渲染层：React + Tailwind v4（`--spacing: .25rem` 基数）+ 全量 CSS 变量主题系统。
- 主题切换用 `--lightningcss-light(...)` / `--lightningcss-dark(...)` 成对函数变量，同一变量同时携带两种主题值，由 `data-theme` / `prefers-color-scheme` 选择。
- 窗口类型用 `data-codex-window-type=electron|browser|chrome-extension` 区分，electron 桌面窗口有专门的字号/字重覆盖块。
- 内置字体仅：OpenAI Sans（3 字重 woff2）、KaTeX 数学字体、carlito。正文完全依赖系统字体栈。

## 2. 字体系统（用户最关注的"对话 + 左侧列表"字体）

### 2.1 字体族

- 正文/UI 栈（`--font-sans-default`，层叠后生效值）：
  `-apple-system-body, ui-sans-serif, -apple-system, system-ui, "Segoe UI", "Helvetica", "Apple Color Emoji", "Arial", sans-serif, "Segoe UI Emoji", "Segoe UI Symbol"`
  - macOS 上实际渲染为 **SF Pro**；`-apple-system-body` 排在首位带来系统 body 语义（跟随系统文本设置）。
  - 文件前部还有一个旧栈 `-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif`（字节偏移 17350），被后部 Codex 主题块（偏移 849252）覆盖。
- 等宽栈（`--font-mono-default`）：`ui-monospace, "SFMono-Regular", "SF Mono", menlo, consolas, "Liberation Mono", monospace`（macOS = SF Mono）。
- 品牌字体：OpenAI Sans 打包了 Regular/Medium/Semibold（400/500/600，woff2，`font-display: swap`），但只通过 `.font-openai-sans` 工具类按需引用（营销位/标题），聊天与侧边栏不用它。
- 衬线：`ui-serif, Georgia, Cambria, "Times New Roman", Times, serif`。

### 2.2 字号（electron 窗口覆盖后的实际值）

| 令牌 | 值 | 用途 |
|---|---|---|
| `--text-xs` | 12px | 辅助小字 |
| `--text-sm` | 13px | 次级 UI 文本 |
| `--text-base` | 14px | 主字号（行高 21px = 1.5） |
| `--text-lg` | 16px | 标题 |
| `--codex-chat-font-size` | `var(--font-ui-size)` = 14px（fallback 13px） | 聊天正文 |
| `--codex-chat-code-font-size` | `var(--font-code-size)` = 13px（fallback 12px） | 内联/块代码 |
| `--diffs-font-size` | 同代码字号 13px | diff 视图 |
| `--diffs-line-height` | `字号 × 1.8` | diff 行高（≈23.4px） |

- Tailwind 原始行高比：`--text-sm--line-height: calc(1.25/.875)` ≈ 1.43；`--text-base--line-height: 1.5`。

### 2.3 字重（关键细节）

- `body { font-family: var(--font-ui-family); font-weight: var(--font-ui-weight); }`
- electron 窗口覆盖 `--font-ui-weight: 430`（默认 `var(--font-weight-normal)` = 400）。
- **430 是 SF Pro 可变字体的非整数中间值**：比 400 稍重、比 500 轻，是小字号下"清晰但不发虚"观感的核心来源。Sidebar 列表、对话正文都继承这个 430。
- `--font-weight-medium: 500` 用于强调。

### 2.4 平台差异（Windows 有没有单独字体设置？）

**结论：没有按操作系统划分的字体分支，只有一套跨平台字体栈 + 按窗口类型的覆盖。**

验证过程（对解包产物全量检索）：

1. 全部 241 个 CSS 中无 `[data-platform=win32]`、`platform-win`、`os-windows` 等平台选择器；字体相关规则零平台分支。
2. 无 `Segoe UI Variable` 显式引用；入口 JS 里存在 `data-platform` 属性注入（`entry-facc441db94c.js`，挂在 `<header>` 上），但只服务于窗口控制按钮区（Win 标题栏布局），不涉及字体。
3. JS 中无按 `win32/darwin` 切换字号/字重的逻辑；`--font-ui-weight: 430` 与字号覆盖（13px/12px）都定义在 `[data-codex-window-type=electron]` 块——**按窗口类型划分，不按操作系统划分**，Windows 上同样生效。

Windows 的适配方式是"同一字体栈按序 fallback"：

- 正文栈第二位的 `"Segoe UI"` 在 Windows 上接管（macOS 上被前面的 `-apple-system-body/-apple-system` 命中）。
- Tailwind preflight 的 `--default-font-family` 栈更全：`-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", "Noto Sans", Arial, sans-serif, ...`，`Roboto`/`Noto Sans` 覆盖 Linux/Android。
- 推断（非反编译事实）：Windows 11 上 `ui-sans-serif` 解析为 Segoe UI Variable，430 字重可真实插值；Windows 10 的 Segoe UI 是固定字重字体，430 会被舍入到 400 渲染——这是把 `ui-sans-serif` 放在栈第二位的实际意义。
- Windows 专属设置存在但不在字体上：`titleBarStyle: hidden` + `titleBarOverlay`（高 36px，symbolColor light #1f1f1f / dark #ffffff）。

## 3. 圆角系统

- 全局缩放：`--codex-corner-radius-scale: 1.25`，所有语义圆角 = 基础值 × 1.25。
- **超椭圆圆角**：`--codex-corner-shape: superellipse(1.5)`（CSS `corner-shape`，Chrome 139+/Electron 支持，iOS squircle 观感）；composer 单独用 `superellipse(1.1)`（更接近圆弧）。
- 基础值阶梯（rem → ×1.25 后）：
  - xs: 2px → 2.5px
  - sm: 4/6px → 5/7.5px
  - md: 6/8px → 7.5/10px
  - lg: 8/10px → 10/12.5px
  - xl: 12px → 15px
  - 2xl: 16px → 20px（用户消息气泡 `rounded-2xl`）
  - 3xl: 20/24px → 25/30px
  - 4xl: 24/32px → 30/40px
- 组件级：
  - composer 输入框：`--composer-radius: calc(var(--spacing) * 7)` = **28px**。
  - 侧边栏会话行：`--radius-token-row: 9999px`（全圆角胶囊）；`.sidebar-item` 用 `radius-lg`（12.5px）+ superellipse(1.5)。
  - `.sidebar-icon-button`：`radius-md`（10px），24×24px。
  - 消息操作按钮：32px 高（`spacing*8`），`radius-lg`。
  - diff 文件行底部：`radius-lg`。

## 4. 间距系统

- 基数 `--spacing: .25rem`（4px），组件全部用 `calc(var(--spacing) * n)` 组合。
- 侧边栏会话行高：`--height-token-nav-row: calc(var(--text-base) * 1.5 + var(--padding-row-y) * 2)` = 21px 文字 + 8px = **29px**（browser 窗口另有 30px/36px 覆盖）。
- 行内纵向 padding：`--padding-row-y: spacing*1` = 4px。
- 消息块间距：`--conversation-item-gap: 16px`；同组消息 4px。
- 工具栏高：`--height-toolbar: 46px`。
- composer：行高 20px（spacing*5）、最小高 44px（spacing*11）、输入水平 padding 12px、placeholder 透明度 0.5。
- 面板基础 padding 12px（spacing*3），工具栏 padding 16px（spacing*4）。
- 用户消息气泡：水平 `--thread-content-margin`（12 或 16px）、纵向 py-2.5 = 10px、`--user-chat-width: min(456px, 100%)`。
- 滚动条/图标：侧边栏图标 24px 见方（spacing*6）。

## 5. 颜色系统

- 主文字 `--color-text`：light `--gray-750` = **#282828**；dark 为中性灰（同令牌 dark 半支）。
- 表面 `--color-surface`：light `--gray-0` = **#ffffff**；dark `--gray-0` dark 支 = **#0d0d0d**。
- 窗口底色（主进程 backgroundColor）：dark `#202020` / light `#ffffff`；quickChat 窗口 `#00000000` 透明。
- 侧边栏背景：**原生 vibrancy `menu` 材质**（macOS 毛玻璃），CSS 侧 `background: 0 0` 透明。
- 灰阶（节选）：gray-50 light #f9f9f9；gray-750 light #282828；gray-900 light #181818；gray-950 light #131313；固定灰 gray-fixed-300 #afafaf、gray-fixed-500 #5d5d5d、gray-fixed-700 #303030。
- 用户消息气泡背景：`--color-background-user-message: color-mix(in oklab, var(--color-text) 5%, transparent)`（文字色 5% 透明度，双主题自适应）；文字色 `--color-text-user-message: var(--color-text)`。
- 语义色板：success/warning/danger/discovery 各有 soft/surface/solid/ghost 四级（`--yellow-50…950`、`--red-…`、`--purple-…`、`--blue-…`）。

## 6. 阴影与层级

- `--elevation-stroke: 0 0 0 .5px var(--color-border-strong)`（0.5px 内描边，所有浮层的底座）。
- prominent（弹层）：stroke + `0 3px 7.5px #0000000a` + `0 0 20px #0000000d`。
- sidebar 面板：stroke + `0 3px 7.5px #00000008` + `0 0 16px #00000005`。
- composer：`0 0 0 1px #0000000a, 0 2px 8px #0000000a, 0 4px 80px 8px #00000006`；dark 追加 `inset 0 0 1px 0 #fff3`。
- 特点：全部低透明度（4%–13% 黑）、大模糊、无饱和色投影。

## 7. 原生窗口层

- 主窗口（darwin）：`titleBarStyle: hiddenInset` + `trafficLightPosition: A9(zoom)`，A9 = `{x:16, y:round((46*zoom-14)/2)}`，zoom=1 → **(16,16)**，即红绿灯在 46px 工具栏内垂直居中。
- `vibrancy: 'menu'`（主窗口 fallback 及 secondary 窗口）。
- Win/Linux：`titleBarStyle: hidden` + `titleBarOverlay`（高 36px，symbolColor light #1f1f1f / dark #ffffff，底色透明）。
- HUD/快捷窗：hiddenInset + trafficLight (10,10)，不可最小化/最大化。
- 启动画面：index.html 内联 loader——56px OpenAI logo，180ms ease-out 淡入，2200ms `cubic-bezier(0.4,0,0.2,1)` shimmer 循环，支持 `prefers-reduced-motion`。

## 8. 复刻到本项目的建议（供参考）

1. 正文直接用 `-apple-system-body` 开头的栈 + `font-weight: 430`，即可在 macOS 上获得与 Codex 一致的 SF Pro 观感；不必打包 webfont。
2. 语义圆角加一层 1.25 缩放系数与 `superellipse()`，是它"圆润但不圆滑失控"的关键。
3. 用户气泡用 `color-mix(in oklab, var(--color-text) 5%, transparent)` 而非固定灰，双主题自动成立。
4. 浮层阴影先加 0.5px 描边再叠两层低透明投影，替代粗边框。

## 附：关键提取来源

- 字体/字号/字重：`app-shared-11c21cbb0024.css`（:root 与 `[data-codex-window-type=electron]` 块）
- 圆角/间距/阴影：同上 `:root` 块群
- 用户气泡类名：`webview/assets/user-message-4c56b67dede4.js`
- 窗口配置：`.vite/build/main-DUHZj4_w.js`（`hiddenInset`/`A9`/`vibrancy`）
- OpenAI Sans @font-face：`app-shared-11c21cbb0024.css` 尾部

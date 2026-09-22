# KeenCode UI 设计规范

> 状态：当前产品规范（规范性文档）
> 更新日期：2026-09-21
>
> 本文描述 KeenCode 当前代码库拥有和能验证的设计规则。界面基线采用经许可的 ZCode 桌面 UI 样式；产品功能、协议、运行时和宿主能力仍由 KeenCode 自身实现。

## 1. 产品定位与设计目标

KeenCode 是本地优先的桌面 AI 编码工具。界面首先服务于重复的编码工作流：选择项目和会话、观察 Agent 回合、处理询问、检查文件与变更、发送下一轮任务。设计目标按以下顺序排列：

1. **可扫描**：在高信息密度下，用户能快速定位当前会话、运行状态、错误和下一步操作。
2. **可控**：运行中、排队中、等待用户输入、失败和已完成状态必须有稳定且可访问的表达。
3. **可恢复**：滚动区域、面板宽度、草稿、资源分栏和设置导航不应因为窗口尺寸或语言变化而丢失上下文。
4. **可迁移**：桌面、浏览器夹具和未来的远程移动控制器共享信息层级，但不假装共享底层能力。
5. **低干扰**：不使用营销式 Hero、装饰性渐变球、无意义的大卡片或持续动效占用工作空间。

页面的主要视觉层级是“窗口壳层 → 工作区分栏 → 内容表面 → 状态与操作”。卡片只用于有实际边界的重复条目、设置分组和对话框，不把整页包成卡片。

## 2. 规范边界与 Source of Truth

### 2.1 当前代码是唯一事实来源

| 范围 | Source of Truth | 约束 |
|---|---|---|
| 颜色、字体、间距、圆角、动效 | `apps/ui/src/styles/tokens.css`、`apps/ui/src/styles/harness/`、`apps/ui/src/styles/skins.css` | 业务 CSS 消费语义令牌，不在组件内复制一套颜色或字号 |
| 样式层叠顺序 | `apps/ui/src/main.tsx`、`apps/ui/src/styles/app.css` | `app.css` 的模块导入顺序保持稳定；治理兜底放在 `ui-governance.css` |
| 页面结构与交互 | `apps/ui/src/App.tsx`、`apps/ui/src/features/app/`、`apps/ui/src/components/` | 本文不能替代组件、路由、状态和协议契约 |
| 原生能力 | `apps/desktop/src-tauri/src/`、`apps/ui/src/lib/tauri.ts`、`apps/ui/src/lib/api.ts` | 浏览器渲染通过不代表 Tauri 原生门禁通过 |
| 图标 | `apps/ui/src/components/icons.tsx`、`@tabler/icons-react` | 新图标先加入稳定的本地导出，不引入第二套图标库 |
| 许可证与外部适配 | `THIRD_PARTY_NOTICES.md`、对应源码目录 | 适配代码保留来源和许可证；产品规范与外部产品规范分离 |

`DESIGN.md` 只规定目标、语义和验收规则。组件的最终尺寸、可见状态和宿主限制以当前代码和测试为准；发现文档与代码不一致时，先核对代码、测试和清单，再更新本文。

### 2.2 外部来源与归属

`docs/research/archive/` 中的研究材料是历史参考，不是 KeenCode 规范，也不作为当前 UI 验收基线。特别是其中的 Electron/Codex 反编译报告：

- 只能帮助理解通用的排版、信息密度和桌面窗口问题；
- 不能证明 KeenCode 使用 Electron、OpenAI Sans、`data-codex-window-type` 或任何同名变量；
- 未经许可证或用户明确授权，不复制外部源码、CSS、提示词、组件实现或产品文案；
- 不能替代本仓库的 Tauri、WebView、浏览器夹具和真实设备验收。

当前 token 文件中存在经许可适配的 DeepSeek Harness 基础样式；深色主题、桌面壳层、侧栏、会话正文和 Composer 采用 ZCode Apache-2.0 UI 样式作为视觉基线。完整归属见 `THIRD_PARTY_NOTICES.md` 与 `LICENSES/`。这些文件是界面实现输入，不等于 KeenCode 的协议、运行时或产品行为来源。

## 3. 宿主能力矩阵

| 能力 | Desktop / Tauri（当前主宿主） | Web 启动壳 / Browser fixture | Mobile Remote Controller |
|---|---|---|---|
| Agent 运行时 | 进程内 ACP Host 与 Tauri command | 登录后由注入的 Host transport adapter 提供 | 由注入的 Host transport adapter 提供 |
| 本地项目、文件、Git、Terminal | 可用，受 Rust 系统能力边界控制 | 不可假定可用 | 禁止直接访问手机或桌面本地文件、Terminal |
| 会话事件与历史 | Rust 权威状态，前端投影 | 可用的仅是夹具或 adapter 投影 | 只展示服务端授权的会话和资源摘要 |
| Embedded Browser / 远程 URL | 桌面专用能力按现有实现开放 | 浏览器自身能力不能冒充产品能力 | 不在首版远程控制器中开放任意浏览器内核 |
| 窗口拖拽、标题栏、托盘 | 原生门禁，必须在对应系统验收 | 不可验证 | 不适用 |
| 主题与界面语言 | 当前 UI 逻辑 | 可用于组件和布局验证 | 必须保留同一语义，不复用桌面专属控件 |

`apps/ui/src/lib/tauri.ts` 在非 Tauri 环境不能提供真实 `invoke`。设置中的 Web Service URL 是模型或服务配置，不是本机 Web Host 地址。Web 启动壳通过同源 `BrowserWebHostTransport` 使用 Token 登录、HttpOnly Cookie 和 WebSocket/ACP；移动启动壳仍只消费注入的 `HostTransportAdapter`，其会话权限、事件重连和资源授权仍属于 Rust/Host adapter 边界。

`apps/ui/src/main.tsx` 解析显式 `hostMode`（query、注入全局或 Vite 配置），并维护 `data-host-mode="desktop" | "web" | "mobile-remote"`；未显式指定时保持 Desktop 行为。

## 4. 版式与页面骨架

### 4.1 Workbench

当前 Workbench 的稳定结构是：

```text
.workbench
├── .sidebar       会话、项目、历史、搜索、排序、归档、置顶、拖放
├── .main          Header、通知、对话、询问、Composer、队列、附件
└── .aside         files / web / changes / terminal / trajectory / agents / subagent
```

- `.workbench`、`.main`、`.aside` 和各自内部滚动宿主必须设置 `min-width: 0`、`min-height: 0`，避免长路径、模型名或代码块撑破分栏。
- 侧栏和资源栏可调整宽度，但不能让中间对话列小到遮挡 Composer 或状态操作。默认尺寸来自 `--sidebar-width`、`--aside-width`，不在组件中再次硬编码。
- `.main` 是主内容表面，不再叠加装饰性卡片。对话、工具回合、错误和询问使用各自的内容层级。
- Composer 是持续工作的主操作区；上下文、附件、队列和工具栏必须能在窄宽度下换行或滚动，不能用横向溢出隐藏关键信息。
- Resource Viewer 的 tab、文件树和预览是独立的内容工作区；代码、Diff、Terminal、Markdown 和 Office 预览保留各自的等宽或内容排版例外。

### 4.2 Settings

Settings 使用导航 + 单一正文滚动区的结构。当前分区为：

`general`、`appearance`、`account`、`personalization`、`skills`、`plugins`、`agents`、`market`、`mcp`、`archive`、`archived`、`requests`、`analytics`、`about`。

- 导航与正文共享标题栏避让规则；正文只有一个主要纵向滚动宿主。
- 设置卡片表达一个可独立理解的设置组，不在卡片内继续嵌套页面卡片。
- 设置行的说明文本允许换行，尾部控件不应挤压或覆盖说明；窄屏时行切换为纵向排列。
- 外部链接、危险操作、保存失败、加载中和空状态都必须有可读的状态文本或辅助技术名称。
- 当前约 `680px` 以下切换设置移动导航。这是 Settings 的窄屏布局，不代表已经存在 Mobile Remote Controller。

### 4.3 标题、状态和操作

- 每个页面或面板只有一个主要标题；标题、副标题和当前项目/会话身份使用稳定的截断策略。
- 主操作使用现有组件变体；图标按钮必须有 `aria-label` 或可见 tooltip，纯图标不能承担无法猜测的动作。
- 加载、空、错误、重试、已完成、未读和等待用户输入是不同状态，不能只用颜色区分。
- 长文件路径、URL、模型引用、分支名和用户输入使用 `overflow-wrap: anywhere` 或明确的滚动宿主；不要把 `white-space: nowrap` 作为全局解决方案。

## 5. 视觉令牌与排版

### 5.1 字体

UI 默认使用系统字体栈 `var(--font-sans)`，当前由 `tokens.css` 提供跨平台中文和 Emoji fallback；代码和 Diff 使用 `var(--font-mono)`。仓库当前没有打包 OpenAI Sans 或其他品牌字体文件，不得在设计文档或组件中暗示存在远程字体资源。

界面字号以 `--ui-font-size` 和派生令牌为唯一输入。基线为 14px，当前用户设置允许在既有范围内调整；辅助文字使用 `--text-xs` / `--text-sm`，标题和紧凑控件使用现有 `--text-lg` / `--text-xl`。聊天正文、代码、Diff、终端、Markdown 和 Office 预览可使用自己的内容令牌，但必须说明原因。

字重、行高和字距服务于可读性：

- UI 和表单控件继承 `var(--font-sans)`、`var(--ui-font-size)` 和 `letter-spacing: 0`；
- 正文行高优先使用 `--leading-normal`，状态和菜单可以使用更紧凑的既有行高；
- 不在业务组件中添加负字距来模拟外部产品；
- 中文、英文、数字、路径和 Emoji 混排时，宽度必须通过真实渲染验收，而不是只看 CSS 数值。

### 5.2 颜色和表面

组件使用语义令牌：`--bg-app`、`--bg-main`、`--bg-sidebar`、`--bg-card`、`--bg-input`、`--border-subtle`、`--border-strong`、`--border-focus`、`--text-primary`、`--text-secondary`、`--text-tertiary`、`--accent`、`--success`、`--warning`、`--danger`。组件内不直接写主题相关的 Hex 颜色。

- Light、Dark 和系统主题是同等重要的产品状态；不要只在一种主题下验收。
- 主操作、链接、成功、警告、错误和信息状态不能只靠色相区分，必须同时提供文字、图标、形状或位置线索。
- 表面层级优先用背景、边框和轻微阴影表达；不要新增大面积模糊、装饰性光晕或单一色相的整页配色。
- 自定义壁纸是现有功能，scrim 和可读性由现有 skin 规则负责；新增页面不得假设壁纸下仍有不透明背景。

### 5.3 间距、圆角和控件

间距优先使用 `--space-1` 至 `--space-5`；圆角优先使用 `--radius-xs`、`--radius-sm`、`--radius-md`、`--radius-lg`、`--radius-composer`。不为每个组件创建新的近似 token。

控件高度必须沿用组件库和现有业务变体。当前存在 24、28、32、36 等不同用途的紧凑控件，治理层不得给所有按钮强行设置固定高度。触摸场景使用最小触控区域 token 和局部布局规则解决，不以放大所有桌面图标为代价。

## 6. 组件来源、所有权与实现规则

### 6.1 来源清单

| 组件或能力 | 当前来源 | 所有权边界 |
|---|---|---|
| Button | `apps/ui/src/components/ui/button.tsx` 的 KeenCode 本地实现 | 复用已授权的 ZCode 几何和语义变体；`icon-*` 是本地语义尺寸，不透传为 Appica 尺寸 |
| Switch、Tooltip、Input、Select、Tabs、Card、DropdownMenu 等 | `apps/ui/src/components/ui/` 对 `@appica/ui-react` 的本地包装或组合 | Appica 原语固定使用 `size="md"` / `inputSize="md"`；仅 Avatar、Thumbnail 允许官方精确像素尺寸，业务层不绕过包装层重做外观 |
| 图标 | `apps/ui/src/components/icons.tsx` + `@tabler/icons-react` | 通过本地稳定名称导出；不手写重复 SVG、不混用第二套图标库 |
| 编辑宿主 | 原生 `textarea`、`contenteditable` | 仅用于编辑和输入宿主；可见外观由现有业务样式管理，并解释浏览器必要性 |
| 终端、Diff、Markdown、Office | xterm、`@pierre/diffs`、React Markdown、`docx-preview` 等已声明依赖 | 内容渲染属于内容例外，不把内容字体规则外溢到普通 UI |
| 颜色、表面和排版 token | KeenCode `tokens.css`、已声明的 Harness 适配与 ZCode UI 样式 | 产品语义由 KeenCode 维护；第三方归属保留在 `THIRD_PARTY_NOTICES.md` 与 `LICENSES/` |

业务代码不得新增可见的原生 `button`、`input`、`textarea`、`select` 或 `dialog` 来绕过现有组件。浏览器要求的隐藏控件、`contenteditable`、媒体和编辑器宿主除外。新增组件前先搜索 `apps/ui/src/components/ui/`、`@appica/ui-react` 和现有业务组件。

### 6.2 修改和归属规则

1. 先写明需求属于布局、语义、交互还是宿主能力，再选择修改 token、组件、业务 CSS 或 Rust/协议层。
2. 外部样式只有在许可证允许且用户明确授权时才能复制或适配；必须保留许可证、来源、版本和使用位置。协议、运行时、提示词、产品文案及业务数据结构仍须独立实现。
3. 新增图片、字体和图标必须说明来源、许可证、体积和加载边界。默认不引入外部字体请求。
4. 业务 CSS 只负责布局、状态组合和产品结构，不覆盖基础控件的颜色、字体、边框和交互状态，除非该覆盖是本产品的明确变体。
5. 视觉修改必须补充同一状态、视口、主题和语言下的截图或说明；浏览器截图不能替代原生 Tauri 门禁。

## 7. Web 与 Mobile 响应式规则

### 7.1 断点和布局状态

| 宽度 | 现有/目标状态 | 规则 |
|---|---|---|
| `>= 921px` | Desktop 工作区基线 | 允许侧栏、主列、资源栏同时存在；保持稳定的分栏和滚动宿主 |
| `681–920px` | Web/小桌面过渡 | 优先保证主列和 Composer；次要工具栏换行，资源栏可按现有状态隐藏或压缩 |
| `461–680px` | 窄屏 | Settings 使用移动导航；Workbench 控件换行或进入明确的面板状态，不能仅靠裁切 |
| `<= 460px` | 手机宽度 | 设置行单列、输入字号至少 16px、触控区域增大、资源 tab 可横向滚动；不显示桌面专属拖拽/托盘提示 |

这些是布局验收状态，不是要求一次性重写三栏 DOM。当前代码已经为 Settings 和部分 Composer/Resource 场景提供窄屏规则；Workbench 的完整移动控制器仍需独立接线。

### 7.2 Mobile Remote Controller 的明确限制

未来移动端只控制远程会话，不继承桌面端权限：

- 只显示会话列表、运行状态、用户询问、消息输入、授权的资源摘要和安全的错误/重试动作；
- 禁止直接启动本地 Terminal、读取任意本机路径、调用 Tauri command 或使用桌面拖拽区；
- 文件预览、Diff 和日志必须来自远程服务明确授权的资源，且有大小、类型和生命周期限制；
- 断线、重连、过期、权限变化和正在执行状态要能被用户识别；
- 未完成传输、认证和远程资源协议前，不在 UI 中展示“已支持移动端”的开关或营销文案。

### 7.3 输入、滚动和安全区

- 可编辑输入在手机宽度使用至少 16px 的实际字号，避免 iOS WebKit 聚焦时自动缩放。
- 横向内容只在明确的 tab、代码、Diff、表格或路径查看器内滚动；普通页面不得出现隐式横向滚动。
- Composer、底部操作区和弹窗考虑 `env(safe-area-inset-*)`，不能把发送、停止、保存或重试按钮放在不可触达区域。
- 中文 IME 组合输入、回车发送、Shift+Enter 换行、粘贴附件和屏幕键盘顶起布局必须在真实 WebView/设备上验收。

## 8. 可访问性、国际化与动效

- 所有可交互元素可通过键盘到达；焦点顺序跟随视觉顺序，焦点环不依赖颜色差异。
- 图标按钮、拖拽调整条、tab、菜单项、加载区、错误区和实时更新区提供语义名称或状态。
- 文字和重要边界遵守 WCAG AA 对比度目标；错误、成功和未读不能只由红绿颜色区分。
- 文案允许中英文、繁体中文和长模型名展开；不要用固定宽度或绝对定位拼接句子。
- 用户输入、路径、URL、代码和模型名保持可复制；应用壳层可以禁止选择，但不能覆盖编辑和内容区域。
- `prefers-reduced-motion: reduce` 下关闭非必要动画和滚动平滑；`forced-colors: active` 下使用系统颜色和可见焦点。
- 动效用于状态变化和空间关系，不用于持续吸引注意；所有动画必须能在失败、卸载和路由切换时清理。

## 9. 资源、字体和 CSP 边界

当前静态资源以 `public/logo.png` 和仓库内资源为主。Tauri CSP 已为本地资源和 `data:` 字体声明边界，但这不等于允许业务代码加载任意远程字体、脚本或图片。

- 新资源放入明确的 `public/` 或业务资源目录，使用稳定的 asset protocol/打包路径；不要把开发机绝对路径写进组件。
- 外部图片、字体和脚本必须有明确需求、许可证、CSP 评估和失败兜底；默认离线可运行。
- 不把 base64 大文件塞入 TSX 或 token 文件；需要二进制资源时记录体积和缓存行为。
- Preview、Embedded Browser 和远程 URL 的安全策略属于宿主和 Rust 边界，不能用 CSS 或浏览器夹具推断权限。

## 10. 性能与 Design QA

当前文档不对流式刷新、P95、CPU、RSS 或内存增长做未经测量的承诺。性能报告必须附环境、视口、主题、语言、会话数量、事件量和采样方式，至少记录：

- 首个可交互窗口时间；
- 首个可见 token 延迟；
- ACP 事件到可见 DOM 更新的中位数、P95 和最大值；
- 长任务数量和持续时间；
- 空闲、单活跃会话和并发会话的 CPU/RSS；
- 低端窗口尺寸下的横向溢出、掉帧、输入延迟和滚动稳定性。

`design-qa.md` 是记录入口，顶部索引说明当前页面、视口、主题、语言、夹具边界和历史记录。每次可见 UI 修改至少执行：

1. `pnpm run typecheck`；
2. 相关 Vitest；
3. `pnpm run lint:css`；
4. `git diff --check`；
5. 可行时用相同 device scale factor 采集 Desktop、Web、窄屏和手机宽度截图，检查 console、横向溢出、遮挡、焦点和主题。

浏览器夹具只能验证 React/CSS/部分交互。Windows WebView2、macOS WebKit、Linux WebKitGTK、无装饰窗口、标题栏拖拽、DPI、中文 IME、Terminal、Embedded Browser、tray 和真实移动设备远程连接必须在相应宿主或 CI 中单独记录。

## 11. Do / Don't

### Do

- 复用已有 token、`@appica/ui-react`、本地 UI 包装和 Tabler 图标导出。
- 让信息密度服务于会话、项目、回合、资源和错误恢复。
- 为长文本、中文 IME、主题、缩放、Reduced Motion 和 forced colors 留出真实布局空间。
- 在文档中区分“当前已实现”“浏览器夹具可测”“未来接线”和“原生未验证”。
- 通过局部治理 CSS 解决跨页面共性约束，避免在 `App.tsx` 堆叠设计规则。

### Don't

- 不把外部 Electron/Codex 研究当作 KeenCode 当前规范。
- 不复制外部组件、CSS、字体、图标、提示词或产品文案。
- 不用固定宽度、负字距、无标签图标或颜色单独表达状态来掩盖响应式和可访问性问题。
- 不把 Settings 的移动导航描述成 Mobile Remote Controller。
- 不把 Web Service URL、浏览器开发服务器或静态截图描述成本机 Agent Host。
- 不在没有基线和指标时声称 16ms/33ms 刷新、P95 改善、CPU 降低或内存无增长。

## 12. 当前实现与验收边界

本次统一接入已经形成一条共享 Host/ACP 核心路径：Desktop、Web、Agent CLI 使用同一套 Session、operation、lease、admission、事件投递和恢复契约；TUI 仅保留 `core/tui` 目录和协议边界，不宣称已有 TUI 界面。Web 由本机 KeenCode Host 提供，不由开发服务器或独立 SaaS 服务提供。

当前产品约束如下：

1. Web Host 使用用户配置的固定端口和 Token，Token 只进入系统凭据存储；轮换 Token 会撤销现有浏览器会话。默认监听 `127.0.0.1`，手机访问必须显式选择本机私有或链路本地地址，Host/Origin 白名单由该地址严格生成，不接受任意 Host Header。
2. WebSocket ACP 在每一个请求上执行方法能力白名单，未知方法、桌面管理、终端和文件越权能力默认拒绝；Session 事件按连接隔离，断线、重连和 Journal 缺口通过有界队列与快照恢复处理。
3. 发布构建把同一份前端静态资源复制到 bundle 的 `web/` 资源目录；Web Host 的裸入口固定进入 `mobile-remote` 宿主模式，不能误挂载桌面 Tauri 能力。
4. Agent CLI 支持 NDJSON 非交互调用、`run`/`session send`/`attach`/`stop`、Ctrl+C、detached operation 和受控 headless Host；未知选项拒绝，Prompt 只有显式 `--` 后才按原文处理。非交互输入请求返回稳定的 `needs_input` 结果和退出码，不等待隐藏的终端输入。
5. 可观测性在 Host、Provider、Agent、工具、前端 Renderer 和 Web transport 边界保留脱敏的 metrics、trace、TTFT、资源采样、启动阶段、Crash spool 和有界实时事件总线；长流渲染采用 RAF 批处理、增量 Markdown、投影缓存和虚拟列表，并对事件、观测和性能状态设置 retention 上限。

仍需在目标环境单独验收、但不属于协议或实现缺口的项目：

- Windows WebView2 原生窗口、DPI、中文 IME、Terminal、托盘、真实 LAN 手机连接和发布安装包的端到端交互；当前浏览器夹具与 Rust 测试不能替代这些宿主验证。
- LAN 使用明文 HTTP/WS；该模式只适用于用户显式开启的可信本地网络。若部署到不可信网络，必须增加 TLS 或可信反向代理，不能仅依赖 Token 声称安全。
- 真实 CPU/RSS、长任务 P95 和 WebView2 内存曲线必须在固定设备、视口、主题、语言、会话数量和事件量下采样；仓库测试只证明容量和生命周期规则，不证明某个机器上的性能数值。

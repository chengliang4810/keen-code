# ZCode 3.14.3 固定像素验收基线

本文定义 `apps/desktop/src/native_ui/` 的 GPUI/Ely 实现必须遵守的 ZCode 3.14.3
视觉基线。固定来源提交为
`29628c9acdb81b703bbd4080c207a0e7ce5e276e`；它是像素级比较基准，而不只是风格
参考。Rust Host、Journal、projection 和 RPC 契约仍以对应协议文档及源码为准。

需要区分来源内容基线与平台窗口基线：`out/native-live/zcode-source-baseline-29628c9-dpr2/`
中的 PNG 来自 Playwright Chromium/Vite 浏览器路径，壳层实际是 `radius=12px`、
`inset=0px`；它可以校准内容布局，但不是 Windows 原生窗口截图。Windows 原生目标的
壳层约束是 `radius=5px`、`inset=4px`。固定来源现已有同平台 Windows 参考运行
`out/ely-native/zcode-windows-reference-5/native-run/report.json`，使用官方
`out/zcode-reference-3.14.3/ZCode.exe`（SHA-256
`afb17a861ce435e1ebecb32fa99ca14daa94adadc6054b5f2116b418248cace5`），其 v3.14.3
annotated tag peeled 到固定提交；最终截图为 `screenshots/0017-workspace-final.bmp`，
metrics 为 `metrics/0018-workspace-final.json`。因此 Windows 几何应以这份同平台参考和
固定 source commit 为准，旧浏览器 PNG 及其差异结果仍不能宣称像素 `PASS`。

## 权威来源

`apps/desktop/src/native_ui/style.rs` 是原生实现的主题、壳层颜色、尺寸和字号权威
来源。业务组件只能通过 Ely `Theme`、`theme.colors` 和这里定义的语义角色取值，不能
使用默认 Ely palette、默认暖白/深棕角色或自行近似的间距来宣称达标。

ZCode 参考的布局和交互来源包括：

- `packages/ui/src/app-shell/WorkspaceShellLayout.tsx`
- `packages/ui/src/WorkspaceSidebar.tsx`
- `packages/ui/src/v4/ConversationHeader.tsx`
- `packages/ui/src/v4/ConversationComposer.tsx`
- `packages/ui/src/v4/ConversationQueuePanel.tsx`
- `packages/ui/src/styles.css`

产品只保留本地工作区、项目/Session 侧栏、会话正文、队列、Composer 和原生工作台。
官方账号、支付、云端、机器人、SSH/WSL/Docker 和浏览器计算机控制不属于迁移范围。

## 固定几何

`style.rs` 的数值是 GPUI 逻辑像素。高 DPI 下由窗口系统负责物理像素缩放，不能把
截图中的物理像素反写为布局值。

| 角色 | 常量 | 固定值 |
| --- | --- | ---: |
| 桌面侧栏宽度 | `SIDEBAR_WIDTH` | `264px` |
| 桌面外沿间距 | `DESKTOP_INSET` | `4px` |
| Windows 原生内容 frame 圆角 | `workspaceShellWindowChrome.ts` Windows 分支 / `main_workbench.rs` | `5px` |
| 浏览器 source 内容 frame 圆角 | `workspaceShellWindowChrome.ts` 浏览器分支 | `12px`（仅内容参考） |
| 浏览器 source 外沿间距 | `WorkspaceShellLayout.tsx` 浏览器路径 | `0px`（仅内容参考） |
| 标题栏高度 | `TITLEBAR_HEIGHT` | `48px` |
| Windows 会话滚动槽 | `CONVERSATION_SCROLL_GUTTER` | `15px`（逻辑像素；消息与 dock 共用） |
| 源 sidebar 导航行 | 导航行几何 | `h32px`；行内 `gap4px`；图标 `16px`；普通文字 `14px`；快捷键 `10px` |
| 独立拖拽槽 | sidebar drag surface | `48px`，不并入导航行或会话列表 |
| 分组工具栏外壳 | group toolbar | `28px` |
| 分组工具栏内部 tab/控件 | group toolbar control | `24px` |
| 普通会话正文和非空 Composer | `conversation_content_width` | `<864px` 使用可用宽度；`>=864px` 为 `min(可用宽度 - 96px, 896px)`；`>=1280px` 为 `min(可用宽度 - 384px, 1152px)` |
| 空草稿 Composer 最大宽度 | `EMPTY_COMPOSER_MAX_WIDTH` | `672px` |
| 设置页内容上限 | `CONTENT_MAX_WIDTH` | `896px` |

`TITLEBAR_HEIGHT=48px` 来自固定来源 `WorkspaceHeader.tsx` 的 `h-12`，同时定义窗口
chrome 的 overlay/拖拽表面与主工作台顶部布局占位。原生 `main_workbench.rs` 必须保留这
段 48px 高度，浮动窗口控制层不能替代它。该高度只能计入一次；其下的消息正文、Composer、
导航行、拖拽槽和会话列表各自保持上表几何，流式文本、回复终态或草稿变化不能重新分配
这些区域。

普通会话正文和非空 Composer 共享 `conversation_content_width` 的断点结果，不能因流式
文本、错误文案、按钮标签或队列刷新而改变。空草稿 Composer 使用独立的 `672px` 上限；
设置页内容使用 `896px` 上限。侧栏、标题栏、内容区、输入区的边界应由这些约束和现有
布局组件共同决定。

## Shell 颜色角色

`ShellColors` 明确区分 `canvas`、`sidebar`、`content`、`header`、`panel` 和 `input`。
它们不能合并为一个通用背景。以下是 `style.rs` 当前的精确来源值：

| 角色 | Light | Dark | 规则 |
| --- | --- | --- | --- |
| `canvas` | `#ececee` | `#2b2b2b` | Windows 窗口画布 |
| `sidebar` | `canvas` | `canvas` | Windows 继承 `canvas`；非 Windows 为 `#f0f0f0` / `#161616` |
| `content` | `#f8f8f8` | `#161616` | 等于 palette `bg` |
| `header` | `#ffffff` | `#202020` | 标题栏表面 |
| `panel` | `#ffffff` | `#202020` | 结构面板表面 |
| `input` | `#ffffff` | `#2b2b2b` | 等于 palette `sunken` |

## 精确 Palette

应用启动时 `install_palettes` 为 Light 和 Dark 都安装 `zcode_palette`，并取消 Ely
默认过渡帧。下表使用源码中的十六进制值和 alpha 表达式；alpha 是在对应颜色上叠加
得到的语义值，不能改写成相近的静态灰色。

| Palette 角色 | Light | Dark |
| --- | --- | --- |
| `bg` | `#f8f8f8` | `#161616` |
| `surface` | `#0d0d0d` @ `3%` | `#ffffff` @ `5%` |
| `sunken` / `overlay` | `#ffffff` | `#2b2b2b` |
| `hover` | `#0d0d0d` @ `5%` | `#ffffff` @ `5%` |
| `active` | `#0d0d0d` @ `5%` | `#ffffff` @ `10%` |
| `border` | `#0d0d0d` @ `10%` | `#ffffff` @ `10%` |
| `border_strong` | `#0d0d0d` @ `15%` | `#ffffff` @ `15%` |
| `fg` | `#262626` | `#d4d4d4` |
| `fg_muted` | `fg` @ `60%` | `fg` @ `60%` |
| `fg_subtle` / `fg_disabled` | `fg` @ `40%` | `fg` @ `30%` |
| `accent` | `#000000` | `#ffffff` |
| `accent_hover` | `accent` @ `85%` | `accent` @ `85%` |
| `on_accent` | `#ffffff` | `#000000` |
| `link` / `info` | `#0b7fff` | `#4099ff` |
| `focus` | `border_strong` | `border_strong` |
| `selection` | `link` @ `22%` | `link` @ `28%` |
| `success` | `#1e8a3e` | `#46bf72` |
| `warning` | `#e07b00` | `#ff8a30` |
| `danger` | `#e03131` | `#ff5c5c` |
| `success_subtle` / `warning_subtle` / `danger_subtle` | 对应状态色 @ `12%` | 对应状态色 @ `12%` |
| `info_subtle` | `#ebf4ff` | `#001d3d` |
| `backdrop` | `#000000` @ `40%` | `#000000` @ `40%` |
| `glass` | `surface` | `surface` |
| `shadow` | `#000000` | `#000000` |
| `tooltip_bg` | `#f0f0f0` | `#2b2b2b` |
| `tooltip_fg` | `#0d0d0d` | `#f8f8f8` |

图表和 ANSI 颜色也由 `style.rs` 的 `palette.chart`、`palette.ansi` 固定；新增组件
不得另建色板。`chart` 的八个槽位按顺序为：

- Light：`info`、`success`、`#9e77ed`、`danger`、`warning`、`#0aa7a7`、`link`、
  `fg_muted`。
- Dark：`info`、`success`、`#7b5ce5`、`danger`、`warning`、`#42c8c8`、`link`、
  `fg_muted`。

ANSI 颜色数组按源码的标准 8 色和亮色 8 色顺序固定：

- Light：`#5c5c5c #e03131 #1e8a3e #e07b00 #0b7fff #9e77ed #0aa7a7 #adadad`；
  `#888888 #e03131 #1e8a3e #e07b00 #0066dd #9e77ed #0aa7a7 #0d0d0d`。
- Dark：`#363636 #ff5c5c #46bf72 #ff8a30 #4099ff #7b5ce5 #42c8c8 #adadad`；
  `#747474 #ff9999 #87d9a4 #ffb26b #80beff #a888f2 #8ee5e5 #f8f8f8`。

语义色只用于真实状态，不能把成功、警告或危险色当作装饰色。

## 字号映射

`UiTextSize` 以 `14px * theme.font_scale` 为基准，再加源码中的固定偏移。以下是
`font_scale = 1.0` 时与来源 `text-ui-*` 对应的值：

| 来源角色 | `UiTextSize` | 偏移 | 默认值 |
| --- | --- | ---: | ---: |
| `text-ui-xs` | `Xs` | `-4px` | `10px` |
| `text-ui-sm` | `Sm` | `-2px` | `12px` |
| `text-ui-caption` | `Caption` | `-1px` | `13px` |
| `text-ui-base` | `Base` | `0px` | `14px` |
| `text-ui-lg` | `Lg` | `+2px` | `16px` |
| `text-ui-xl` | `Xl` | `+4px` | `18px` |

字号缩放只能沿用 `Theme.font_scale`。外围控件、标题、标签和元数据使用这组语义角色；
代码、Diff、终端内容可以使用自身的数字字体设置，但不能扩展成第二套界面字号系统。

Windows 原生启动明确使用 GPUI `TextRenderingMode::Grayscale`，对齐固定 ZCode 参考的
灰度文字边缘。release34 的四个相同文案区域中，来源的 RGB 通道差大于 2 的像素均为
0，GPUI 平台默认 ClearType 分别为 1244、755、3280 和 1051；这些彩边属于应用渲染模式
差异，不能用“跨渲染器无法完全相同”跳过修正。灰度模式仍不保证字形栅格化逐像素一致，
是否改善必须以新的二进制和完整图像比较为准，也不修改系统字体平滑设置。

页面默认行高继承 Tailwind preflight 的 `1.5`；设置卡片是例外。固定来源
`components/ui/card.tsx` 使用 `text-ui-base/relaxed`，标题实际继承 `1.625`，默认
14px 字号对应 22.75px；说明显式使用 `leading-6`，仍为 24px。单行标题和单行说明的
设置行连同 `mt-1`、`py-3` 和分隔边框共 75.75px，DPR2 下分隔线间距应交替为
151/152 个物理像素。GPUI `Text` 在物理像素上 snap 行高与 padding，默认 DPR2 标题
变成 22.5px。原生普通说明行按同一卡片累计边界分配底部 padding 误差；不对复杂控件
主导高度的行应用该补偿。不能把 21px 页面默认行高用于卡片，也不能固定标题高度来压住长文字。

Composer 默认边框使用 `border`，悬停和正文输入焦点使用 `border_strong`。一直使用
较强边框会让 Dark 输入壳比来源更亮；该差异不能归因于字体抗锯齿。

## 组件与交互约束

- 侧栏导航项由 `sidebar.rs` 的真实 handler 接线到工作台或设置行为，并从 Host projection
  读取项目/Session 状态；本基线中的导航描述对应当前可操作入口，新增项必须提供同等真实接线。
- 侧栏行保持稳定高度和键盘焦点；消息动作默认只在对应行 hover 或 focus 时显露，并
  保留焦点状态下的可见回退。
- Composer 使用 Enter 提交、Shift+Enter 换行；发送中以停止动作替代发送动作，状态
  来自 Session projection。
- 输入草稿、队列、附件、反馈和回退只能由 Host receipt/event 更新，不能用按钮状态或
  optimistic overlay 冒充已确认事实。
- 面板使用语义边框和表面区分层级；不添加装饰性渐变、光斑、大面积品牌填充或重复套
  叠的卡片。
- 文本容器必须允许长中文和英文自然换行或滚动，不能用 `nowrap` 裁切按钮、菜单项或
  状态文案。

## 当前验证证据

`out/ely-workspace-tests-12.log` 中记录的测试组均为 `test result: ok`；
`out/ely-workspace-clippy-18.log` 和 `out/ely-workspace-clippy-19.log` 均完成检查。
`out/ely-native/smoke-20/native-run/report.json` 记录输入后 Enter 未产生 `turn_started`
并因 Journal 等待超时失败，配套 `screenshots/0009-typed.bmp` 显示输入前缀丢字。
这些记录不能提升产品功能、像素或性能状态，三者仍保持 `pending`。

## 像素验收

像素比较前必须固定以下条件：

1. 使用同一 Light 或 Dark 主题以及同一 `style.rs` palette。
2. 使用同一逻辑窗口尺寸、同一窗口状态和同一 DPI 缩放。
3. 使用同一字体、字体加载状态和 `Theme.font_scale`。
4. 使用同一页面状态、项目/Session 数据、滚动位置、焦点和队列状态。
5. 截图必须来自同一平台的真实窗口 client 区；浏览器 source PNG 没有 HWND、原生
   window/client rect 或 Windows 最大化证据，只能用于内容几何校准。固定 source 的
   Windows reference-5 证据必须记录 binary SHA、DPI、client 尺寸和页面状态。

验收证据至少包括当前原生窗口截图或像素 diff，以及对应运行日志或验收报告。历史截图、
静态源码映射、不同主题/DPI/字体的截图都不能证明像素一致。没有满足条件的当前证据时，
该项状态必须保持 `pending`，不得把编译、静态检查或浏览器预览写成像素验收通过。
当前几何文档和窗口/Journal 校准证据仍不构成目标实现像素 `PASS`；现有 dpr2 浏览器
PNG 与 Windows 原生目标之间已经确认存在 `12px/0px` 对 `5px/4px` 的平台壳层差异。
Windows reference-5 只建立 fixed-source 同平台参照，目标实现仍需在相同页面状态下与
该参照截图或 diff 比较，相关状态满足上述条件前保持 `pending`。

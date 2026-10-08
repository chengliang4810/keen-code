# ZCode 应用外壳 / 会话管理 / 设置 视觉规格提取

来源：`/Users/chengliang/code-repositories/ZCode`（`packages/ui`），全部结论直接取自源码，引用格式为 `文件:行`（均在 `packages/ui/src/` 下，下文省略前缀）。本文件为 Go 版外壳迁移的视觉基线参考。

---

2026-10-06 核对：令牌名不等于最终表面。会话主面板实际使用 `bg-background`（暗 `#161616`、亮 `#f8f8f8`）；`header/panel` 令牌的 `#202020` 不能替代这个调用点。macOS 侧栏透出窗口 `background-alt` 与 vibrancy，不能直接按 `--color-sidebar` 判断截图颜色。

2026-10-06 三栏实施：按用户当前范围，Go版左侧仅新建对话、置顶、项目、对话、底部设置，省略搜索、自动化和自定义分组；终端统一放右侧，与差异、文件并列标签。左侧固定264 DIP，右侧默认360 DIP且可调整，窄窗交互采用简化规则。下文仍记录ZCode原始规格，不代表Go已全部实现；当前实现、原生截图与限制见 [三栏验收](workbench-2026-10-06.md)。

## 1. 应用外壳

### 1.1 窗口整体布局

- 入口 `App.tsx` 只做状态编排，真正布局在 `app-shell/WorkspaceShellLayout.tsx`：`App.tsx:1125-1277` 把全部 props 传给 `WorkspaceShellLayout`。
- 最外层是 `DesktopWindowFrame`（`app-shell/WorkspaceShellLayout.tsx:1511`），内部结构（`DesktopWindowFrame.tsx:31-48`）：
  - 根容器：`flex h-dvh flex-col overflow-hidden border-border text-foreground`（`DesktopWindowFrame.tsx:35`）。
  - 背景：Web/Windows/Linux 用不透明 `bg-background-win-alt`；macOS 桌面用半透明 `bg-background-alt`（配合 vibrancy 磨砂）（`DesktopWindowFrame.tsx:43`）。
  - Linux 桌面额外加 `rounded-[16px] [clip-path:inset(0_round_16px)]`（外层 16px 圆角），最大化时归零（`DesktopWindowFrame.tsx:39-40`）。
- Shell 主行：`relative flex h-full min-h-0 w-full overflow-hidden`，横向三段 = 侧栏面板 + 4px 透明拖拽槽 + 主内容列（`app-shell/WorkspaceShellLayout.tsx:1519-1528, 1530, 1611, 1635`）。
- 桌面端主区面板四周留 4px 内边距：`hasDesktopPanelInset`（mac/win/linux 均为 true），主内容列 `p-1 pl-0 pt-0`，顶部另加 `h-1 w-full [app-region:drag]` 拖拽条（`app-shell/WorkspaceShellLayout.tsx:342, 1640, 1644-1646`）；侧栏收起时其槽位宽度为 4px（`:354`）。
- 内层面板圆角半径 `--workspace-panel-radius`（`resolveWorkspaceShellPanelRadiusPx`，`app-shell/workspaceShellWindowChrome.ts:12-18`）：Windows 5px；macOS ≥26（Tahoe）12px、更早 6px；其余（Linux/Web）12px。内层面板统一带 `border border-border`（`app-shell/workspaceShellWindowChrome.ts:23-48`）。
- 自动收起阈值：会话列宽 < 360px 自动收起左侧栏（`CONVERSATION_AUTO_COLLAPSE_SIDEBAR_WIDTH_PX`，`app-shell/WorkspaceShellLayout.tsx:117, 471`）；< 480px 自动收起右侧面板（`:108, 495`）。

### 1.2 侧栏（左）

- 宽度：默认/最小 264px（`WORKSPACE_SIDEBAR_DEFAULT_WIDTH_PX` / `WORKSPACE_SIDEBAR_MIN_WIDTH_PX`，`app-shell/WorkspaceShellLayout.tsx:99-100`），最大为容器 50%（`:101`），持久化到 localStorage key `zcode:workspace-shell:sidebar-width-px`（`:102`），键盘步进 16px（`:105`）。
- 侧栏面板容器：`w-[var(--workspace-sidebar-panel-width)] max-w-[50%] flex-none overflow-hidden transition-[width,opacity] duration-200 ease-out`，隐藏时 `opacity-0 pointer-events-none`（`app-shell/WorkspaceShellLayout.tsx:1536-1539`）。
- 背景：`<aside>` 本身不设背景（`flex h-full flex-col overflow-hidden`，`WorkspaceSidebar.tsx:1252-1255`），透出外层 frame 的 `bg-background-alt`（macOS 浅灰半透明）/ `bg-background-win-alt`（Win/Linux）。主题变量：亮色 `--color-sidebar: #f0f0f0`（zai-light，`styles.css:485`）/ `neutral-100`（默认亮，`styles.css:182`）；暗色 `--color-sidebar: #161616`（zai-dark，`styles.css:630`）/ `neutral-950`（默认暗，`styles.css:332`）。主区面板为 `bg-background`（`app-shell/WorkspaceShellLayout.tsx:1672`），与侧栏形成"侧栏灰、主区白/黑"的两层结构。
- 分隔/拖拽条：`role="separator"`，宽 `w-1`（4px），透明背景；悬停/拖拽时通过 `after:` 伪元素显示 `w-0.5`（2px）圆角竖线 `bg-foreground-subtlest/50`，纵向 inset 为 `--workspace-resize-handle-inset`（= 面板圆角 + 4px，`workspaceShellWindowChrome.ts:25-27`）（`app-shell/WorkspaceShellLayout.tsx:1626-1631`）。
- 侧栏内部（`WorkspaceSidebar.tsx:1252-1698`）自上而下：
  1. 顶部 `h-12 [app-region:drag]` 拖拽区（`:1257`）。
  2. 操作区：`px-2 py-3`（Windows 为 `py-2`）纵向 `gap-1`（`:1266`）：新建任务行、命令中心搜索行、定时任务、插件市场，均为 ghost 大按钮 `w-full justify-start gap-2 text-foreground hover:bg-surface-hover`（`:1286-1348`）。
  3. 可滚动任务区：`flex-1 overflow-y-auto`，内容 `px-2`、分区间 `gap-3`（`:1353-1358, 1378`）。
  4. 底部 footer：`px-4 pt-2 pb-4`，头像 + 设置齿轮按钮（`WorkspaceSidebarFooter.tsx:217, 371-384`）。
- 顶部浮层 `DesktopTopOverlay`：绝对定位左上，`h-14`（Win/Linux `h-12 top-1 mt-px`），`pointer-events-none` 容器 + `[app-region:no-drag]` 按钮组；含侧栏开关、前进/后退（`size-4` 图标）、新建任务（侧栏隐藏时 `w-7 opacity-100` 展开，否则收成 `w-0`）、更新状态按钮（`DesktopTopOverlay.tsx:102-218`）。macOS 非全屏时按窗控宽度左缩进（默认 96px，`:88-91`）。

### 1.3 主区结构

- 主内容列 `id="content"`：`flex min-w-[320px] flex-1 flex-col`（`app-shell/WorkspaceShellLayout.tsx:1638-1639`）。
- 纵向两层 `ResizablePanelGroup`（垂直方向）：上方会话区（`minSize 35%`），下方终端面板（展开高度 `30%`，`useAnimatedResizablePanel({ expandedSize: "30%" })`，`:410-414`）；横向再与右侧 side pane 分栏（side pane 默认展开 `45%`，`app-shell/sidePaneLayout.ts:1`；会话面板默认 52%、最小 35%，`:1653-1656, 1664-1667`）。
- 会话区容器：`relative flex h-full min-h-0 flex-1 flex-col overflow-hidden bg-background`；side pane 打开时 `rounded-[var(--workspace-panel-radius)] border border-border`，否则使用窗口 chrome 圆角类；终端打开时补 `rounded-b-… border-b`（`:1669-1685`）。

### 1.4 顶栏（WorkspaceHeader）

- 位于会话区顶部：`flex w-full shrink-0 h-12 border-b`，task 态 `border-border/50`，draft 态 `border-transparent`（`WorkspaceHeader.tsx:139-142`）；内部行 `h-12 flex-1 items-center justify-between gap-2 overflow-hidden p-2 [app-region:drag]`（`:157`）。
- 左侧标题区（task variant）：项目名 + 会话标题 + 变更摘要 + Git 信息（`WorkspaceHeader.tsx:162-196`）；draft variant 左侧留空（`:197-199`）。
- 右侧动作区：终端开关、右侧面板开关等图标按钮（`WorkspaceHeader.tsx:200-220`；按钮实现 `WorkspaceHeaderSections/WorkspaceHeaderActionSection.tsx:61-75`）。
- 侧栏隐藏时 header 为窗控留白：macOS `pl-58`（全屏 `pl-38`，有更新按钮 `pl-66`/`pl-48`）、其他平台 `pl-38`（更新 `pl-44`）（`WorkspaceHeader.tsx:122-133`）。

---

## 2. 会话列表（任务列表）

### 2.1 新建会话按钮

- 位置：侧栏顶部操作区第一项（`WorkspaceSidebar.tsx:1266-1285`），非按钮化组件而是一整行可点击组：`w-full h-8 rounded-lg inline-flex items-center pl-2.5 pr-2.5 hover:bg-surface-hover`，左侧 `MessageCirclePlus` 图标（16px）+ 文案 `text-ui-base`（14px 基准），右侧快捷键 `text-ui-xs`（10px）`text-foreground-subtlest`（`NewTaskButtonGroup.tsx:32-44`）；禁用态 `cursor-not-allowed text-foreground-subtlest hover:bg-transparent`（`:34-36`）。点击进入草稿态而非立即建任务（`TaskList.tsx:118-126`）。
- 分组标题行内的新建入口：分区标题右侧 `size="icon-sm"`（24px）ghost 按钮，`size-3.5` 图标，`text-foreground-subtle hover:text-foreground`（Projects 分区 Plus：`WorkspaceSidebar.tsx:1456-1468`；Conversations 分区 MessageCirclePlus：`:1604-1616`）。
- 列表内旧式新建块（workspace 行内展开的 TaskList）：`border-b p-3` 包裹同一按钮组（`TaskList.tsx:412-425`）。

### 2.2 列表项（MemoTaskItem / `TaskListItem.tsx`）

- 行容器 `<li>`：`group/task-item flex cursor-pointer gap-2 rounded-lg pl-2.5 pr-1 py-1 transition-[background-color,border-color,box-shadow]`，默认单行 `items-center`（带工作流运行行时 `items-start`）；timeline 变体 `items-start py-1.5`（`TaskListItem.tsx:542-551`）。行高约 32px（24px 标题行 + 上下 4px padding，见 `:544, 560` 注释）。
- 选中态：`bg-selected`（`isActive ? "bg-selected" : "hover:bg-surface-hover"`，`TaskListItem.tsx:550`）。`--color-selected` = 前景 10% 透明叠色（亮 `rgba(13,13,13,.05)` zai-light `styles.css:482`、默认亮 10% `styles.css:179`；暗 zai-dark `rgba(255,255,255,.1)` `styles.css:627`）。
- 悬停态：`hover:bg-surface-hover` —— `--color-surface-hover`：zai-light `rgba(13,13,13,.05)`（`styles.css:487`），zai-dark `rgba(255,255,255,.1)`（`styles.css:632`）。列表不用分隔线，靠"留白 + 行自身圆角底色"分层（`TaskListItem.tsx:553-555` 注释），行间 `space-y-0.5`（`TaskList.tsx:450`）。
- 前置状态槽：16px（`size-4`）居中槽（`TaskListItem.tsx:557-563`）：error = 6px 圆点 `bg-destructive`（`:573`）；unread = 6px 圆点 `bg-sky-500 dark:bg-sky-400`（`:574-578`）；运行中 = `LoaderIcon size-4 animate-spin`（`:579-580`）；timeline 空闲 = 6px `bg-border` 圆点（`:581-582`）；无状态时被置顶 Pin 图标按钮（`size-4`）替代（`:489-507`）。
- 标题：`text-ui-base text-foreground`（14px），溢出用 mask 渐隐（`TaskTitleOverflowText`）而非省略号，与 grouped 行一致（`TaskListItem.tsx:622-629, 714-721`）；无标题回退 "未命名" 文案（`:245-249`）。
- 变更摘要：标题后 `+N` / `−N`，`text-diff-added` / `text-diff-removed`（`TaskListItem.tsx:402-412`）。
- 时间：右侧 `text-ui-sm text-foreground-subtle`（12px），相对时间——<1min "刚刚"、<60min N 分钟前、<24h N 小时前、其余 N 天前（`TaskListItem.tsx:746-772`；格式化 `lib/taskListItemPresentation.ts:36-53`）；定时任务在时间前显示 `Clock size-3.5`，闲时任务显示 `Moon size-3.5`（`:755-770`）。
- hover 动作入口：hover/焦点时右侧出现动作组（`gap-0.5`）：文件树（`ListTree size-3.5`）+ 归档（`Archive size-3.5`；远端任务用 `CloudUpload`），均为共享 `TaskRowActionButton`（`TaskListItem.tsx:418-488`）；归档是两段式——首击进入待确认态、原位换成 destructive "确认"按钮（`px-2 border-destructive/20`），Esc/点外部取消（`TaskList.tsx:128-150`；确认按钮 `TaskListItem.tsx:422-440`）。
- 重命名：右键上下文菜单（列表级单例 ContextMenu，`TaskList.tsx:442-467`）→ `TaskRenameDialog`（输入框 + 取消/确认，见 §4）；菜单还含置顶/标记未读/复制路径/在分屏打开等（`TaskListItem.tsx:893-958`）。
- 注意/交互徽标：等待输入时显示 `Badge h-5 px-2`，`bg-success/14 text-success`（暗色 `dark:bg-success/18`）（`TaskListItem.tsx:636-640, 739-743`）。

### 2.3 列表分组

- 侧栏任务区分四个视图（`WorkspaceSidebar.tsx:207-220, 1379-1417`）：archived（归档平铺）、grouped（按项目分组）、timeline（时间线）、workspace（按项目分区 = 默认）。
- 默认 workspace 视图：两个可折叠分区 Projects / Conversations（`WorkspacePurposeSection`，可拖拽排序），标题行 `h-7`，标题按钮 `px-2.5 text-ui-base font-medium text-foreground-subtlest hover:text-foreground`，折叠箭头 `size-3.5` 仅 hover 显示，右侧拖拽手柄 + 新建图标按钮 hover 显示（`WorkspaceSidebar/WorkspacePurposeSection.tsx:57-95`；分区顺序持久化 `lib/sidebarPurposeSectionPreferences`，`WorkspaceSidebar.tsx:1425-1430`）。
- 已置顶区：独立于视图常显，标题 `h3 px-2.5 py-1 text-ui-base font-medium text-foreground-subtlest`（`WorkspacePinnedTasksSection.tsx:42-44`），折叠上限 20 条，超出显示 "更多"（`pl-8.5 text-ui-base text-foreground-subtlest hover:text-foreground-subtle`，`:94, 745-758`）。
- 时间线分组：今天 / 昨天 / N 天前 / 本周 / 上周 / 本月 / 上月 / 更早（`lib/taskTimelineGroups.ts:9-16, 52-77`）；组标签 `px-3 pt-2 pb-1 text-ui-base font-medium text-foreground-subtle`（`WorkspaceTimelineTasksSection.tsx:674-684`）。
- 项目行（workspace 列表项）：`ul space-y-2 pb-4`，可拖拽排序（`WorkspaceSidebar.tsx:1509-1562`）；拖拽预览 `rounded-lg border border-border bg-background shadow-lg h-8 px-2.5`（`:182-199`）。

---

## 3. 设置区视觉语言

- 独立页面（tab 切换），同样包在 `DesktopWindowFrame` 内（`SettingsPage.tsx:1364-1372`）。
- 布局：CSS Grid 两列 `grid h-screen grid-cols-[68px_minmax(0,1fr)] lg:grid-cols-[268px_minmax(0,1fr)]`，单行 `minmax(0,1fr)`（窄屏收成 68px 图标栏）（`SettingsPage.tsx:1373-1375`）。
- 左导航 `<aside>`：顶部 `h-12` 拖拽区，`px-2 pb-3 pt-3` 的返回按钮（ghost lg，`rounded-xl`），导航 `flex-1 overflow-y-auto px-2 pb-3`；分组的组标题 `px-2.5 pb-1 text-ui-sm font-medium text-foreground-subtlest`，组间距 `space-y-4`、项间距 `space-y-1`（`SettingsPage.tsx:1400-1500`）。
- 导航项 `SettingsSidebarButton`：`h-8 w-full items-center gap-2 rounded-xl px-2.5 text-left`；激活 `bg-surface-hover text-foreground`，非激活 `text-foreground-subtle hover:bg-surface-hover hover:text-foreground`；图标 `size-4 text-foreground`；窄屏变 40px 方形仅图标（`SettingsPage.tsx:230-257`）。底部"新手指引"入口带虚线边框 `border border-dashed border-border hover:border-border-hover`（`:1509-1512`）。导航栏底部复用 `WorkspaceSidebarFooter`（`:1515-1531`）。
- 右内容面板：`flex flex-col min-h-0 h-full border border-border bg-background`，圆角 Windows `rounded-[5px]`、其余 `rounded-xl`（`SettingsPage.tsx:1560-1566`）；桌面 `p-1 pl-0 pt-0` 与主区一致（`:1554-1557`）。面板内：`h-12` 面包屑行（`SettingsHeaderBreadcrumb`）→ `main overflow-y-auto [scrollbar-gutter:stable]` → 内容列 `mx-auto w-full max-w-4xl px-4 pb-8 pt-0 lg:px-8 lg:pb-10`（`SETTINGS_FRAME_CONTENT_CLASSNAME`，`settings/SettingsPageParts.tsx:21-22`）纵向 `gap-8`（`SettingsPage.tsx:1643-1659`）。
- 页面大标题：`text-2xl font-semibold tracking-tight text-foreground lg:text-3xl`（`SettingsPage.tsx:1663-1667`）；标题旁 Beta 徽标 `h-6 rounded-full border border-sky-500 px-2 text-ui-xs font-semibold text-sky-500`（`:1668-1675`）。
- 分区内容 = 卡片组 `SettingsGroupCard`：`Card rounded-xl border border-border bg-card py-0 shadow-none`（`SettingsPageParts.tsx:148-154`），组间 `space-y-4`（`settingsPageHelpers.tsx:280`）。
- 表单行 `SettingsRow`：`border-t border-border px-4 py-3 first:border-t-0`；两列 grid `grid-cols-[minmax(0,1fr)_192px]`（wide 控件 `sm:grid-cols-[minmax(0,1fr)_280px]`）`gap-4`；左列 label `text-ui-base font-medium text-foreground` + 描述 `mt-1 text-ui-base leading-6 text-foreground-subtle`；右列控件右对齐（`SettingsPageParts.tsx:109-146`）。
- 控件规格：
  - 输入框 Input：`border border-input-border bg-input text-foreground`，hover `border-input-border-hover`，focus `border-input-border-focused bg-input-focused ring-0`；尺寸 default `h-7 rounded-md px-2`，lg `h-8 rounded-lg px-3`（`components/ui/input.tsx:9-22`）。圆角令牌亮色 = `rgba(13,13,13,.1)` 边（`styles.css:479, 497-499`）。
  - 开关 Switch：default `h-[18px] w-[32px]`，sm `h-[16px] w-[28px]`；开 `bg-primary`、关 `bg-primary/30`；thumb 白色 `size-4`（sm 3.5），开时右移（`components/ui/switch.tsx:13-34`）。primary 在 zai 主题下是纯黑/纯白（`styles.css:545-546, 690-691`）。
  - 下拉 Select：trigger 默认 `variant="input"`：`border-input-border bg-input`，尺寸 default `h-7 rounded-md pl-2 pr-1`、lg `h-8 rounded-lg pl-3 pr-2`，右侧 `ChevronDown size-3.5`（`components/ui/select.tsx:14-44, 63-98`）；弹层 `rounded-lg border border-popover-border bg-menu p-1 shadow-md`，选项内部再 `rounded-md`（`:100-120`；选项圆角注释 `:99`）。设置页常用 `size="lg" className="w-[260px] min-w-0 justify-between"`（`settingsPageHelpers.tsx:290-298, 411`）。
- 设置页徽标 pill：`rounded-md bg-surface px-2.5 py-1 text-ui-base font-medium text-foreground-subtle`（`SettingsPageParts.tsx:156-162`）。

---

## 4. 对话框与 Toast

### 4.1 Dialog 基础（`components/ui/dialog.tsx`）

- 遮罩：`fixed inset-0 z-50 bg-black/60 supports-backdrop-filter:backdrop-blur-xs`，100ms 淡入淡出（`dialog.tsx:28-40`）；紧凑确认框可换 `bg-black/20 + blur 2px`（`ConfirmDialog.tsx:99-101`）。
- 内容：居中 `top-1/2 left-1/2 -translate-1/2`，`rounded-2xl border border-popover-border bg-popover p-4 shadow-md`，宽 `max-w-[calc(100%-2rem)]`，gap-4，100ms zoom-95 进出场，`[app-region:no-drag]`；右上角 ghost `icon-sm` 关闭按钮位于 `top-4 right-4`（`dialog.tsx:58-81`；圆角规范注释 `:42`）。
- 标题 `text-ui-base font-medium text-foreground`；描述 `text-ui-base/relaxed text-foreground-subtle`（`dialog.tsx:117-140`）。
- Footer：`flex flex-col-reverse gap-2 sm:flex-row sm:justify-end`（取消在左、确认在右）（`dialog.tsx:93-115`）。

### 4.2 确认框（`ConfirmDialog.tsx`）

- 内容 `rounded-2xl bg-popover/98 p-5 ring-border shadow-2xl gap-5`，普通 `sm:max-w-md`；紧凑变体 `top-[44%] min-h-[161px] sm:max-w-[400px]`（`ConfirmDialog.tsx:102-108`）。
- 标题 `text-ui-lg font-semibold`（16px），描述 `text-ui-base leading-6`（`ConfirmDialog.tsx:110-123`）。
- 按钮排布：取消 = secondary/outline，`h-9 px-4 min-w-28`，右侧内嵌 `esc` 键位提示 `font-mono text-ui-base text-foreground-subtle`；确认 = autoFocus、default（危险操作 `confirmVariant="destructive"`），`min-w-32`，内嵌 `⏎` 提示（`ConfirmDialog.tsx:150-190`）。可选左侧复选框行（`:133-146`）。

### 4.3 重命名对话框（`TaskRenameDialog.tsx`）

- `max-w-xl rounded-2xl p-0`，内部 `p-6 gap-6`；Input `size="lg"`；footer 右对齐：取消 secondary + 确认 default，均 `h-10 px-5`（`TaskRenameDialog.tsx:27-95`）；Enter 确认且忽略输入法组合态（`:56-74`）。

### 4.4 Toast（`components/ui/toast.tsx`，无第三方库、portal 到 body）

- 默认位置 top-center：`fixed top-16 left-1/2 -translate-x-1/2`，栈内 `gap-2`；top-right `right-4 top-16`；bottom-left `left-4 bottom-[calc(1rem+env(safe-area-inset-bottom))]`；bottom-center 居中同高（`toast.tsx:47-59`）。默认 3s 自动消失、200ms 过渡（`:70-71`）。
- 外观：`rounded-2xl border bg-toast/60 backdrop-blur-xl shadow-lg text-ui-base`；default 变体 `px-4 py-3 whitespace-pre-line`；update 变体宽 `min(300px, 100vw-1rem)`、左下角 `size-2 bg-primary/80 ring-4 ring-accent` 圆点 + 标题/副标题 + 右侧 `h-7 rounded-md bg-secondary px-2` 动作按钮；info/warning 变体宽 `min(536px, 100vw-2rem)`，前缀 `TriangleAlert`（warning，`text-warning`）或 `Info` 图标，动作按钮为下划线文字，可带 `size-6` 关闭钮（`toast.tsx:311-401`）。进出动画 200ms `cubic-bezier(0.77,0,0.175,1)`：顶部 -1px/0.98 缩放淡入，右侧从 `translate-x-[calc(100%+1rem)]` 滑入（`:313-331`）。

---

## 5. 亮暗主题关键差异

- 机制：`useTheme.ts` 支持 `system | zai-light | zai-dark`（旧 light/dark 归一化为 zai-\*，默认 `zai-dark`，`useTheme.ts:21-25, 86`）；`applyTheme` 在 `<html>` 上切 `dark` / `theme-zai-light` / `theme-zai-dark` 三个 class（`useTheme.ts:58-70`）。`.dark`（`styles.css:308`）、`.theme-zai-light`（`:461`）、`.theme-zai-dark`（`:606`）三组变量覆盖。产品默认走 zai 两套主题，以下列 zai-light ↔ zai-dark 差异。
- 基底：亮 `--color-background: #f8f8f8`（`styles.css:464`）↔ 暗 `#161616`（`:609`）；窗口外框亮 `#ececee`（`:465`）↔ 暗 `#2b2b2b`（`:610`）；侧栏亮 `#f0f0f0`（`:485`）↔ 暗 `#161616`（`:630`）；header/panel 亮 `#ffffff`（`:483-484`）↔ 暗 `#202020`（`:628-629`）；卡片/浮层/输入底 亮 `#ffffff`（`:488, 491, 495`）↔ 暗 `#2b2b2b`（`:633, 636, 640`）。
- 品牌与主按钮（黑白反转）：`--color-brand` 亮 `#000000`（`:467`）↔ 暗 `#ffffff`（`:612`）；`--color-primary` 亮 `#000000` / 前景 `#ffffff`（`:545-546`）↔ 暗 `#ffffff` / 前景 `#000000`（`:690-691`）。开关、主按钮、选中 tabs 皆随此反转。
- 文本：`--color-foreground` 亮 `neutral-800`（`:553`）↔ 暗 `neutral-300`（`:698`）；subtle/subtlest 为 60%/30% 透明度混色（`:554-555, 699-700`）。
- 边框/悬停/选中：border 亮 `rgba(13,13,13,.1)` ↔ 暗 `rgba(255,255,255,.1)`（`:479, 624`）；hover 5% 黑 ↔ 5% 白（`:481, 626`）；selected 5% 黑 ↔ 10% 白（`:482, 627`）；surface 3% ↔ 5%（`:486, 631`）；surface-hover 5% ↔ 10%（`:487, 632`）。注意 selected/hover 透明度两主题不对称（5%↔10%）。
- 语义色：destructive 亮 `#e03131` ↔ 暗 `#ff5c5c`（`:561, 706`）；success 亮 `#1e8a3e` ↔ 暗 `#46bf72`（`:557, 702`）；warning 亮 `#e07b00` ↔ 暗 `#ff8a30`（`:563` 附近与 `:654`）；dangerous 前景统一白（暗色注释 `styles.css:707-708`）。
- 查找高亮：亮 `#fff4eb` / 激活 `#ffb26b`（`:477-478`）↔ 暗 `#542500` / `#ff8a30`（`:622-623`）；accent 亮 `#ebf4ff` ↔ 暗 `#001d3d`（`:476, 621`）。
- 未读点用 Tailwind 原生 `bg-sky-500 dark:bg-sky-400` 切换（`TaskListItem.tsx:577`），不走令牌。
- 终端/图表色板两套独立十六进制（亮 `styles.css:506-539` ↔ 暗 `:651-684`），主色由 `#0b7fff` 系（亮）换为 `#4099ff` 系（暗）。

---

## 附：读取清单（本会话实际读取并引用的源文件）

`App.tsx`、`app-shell/WorkspaceShellLayout.tsx`、`app-shell/workspaceShellWindowChrome.ts`、`app-shell/sidePaneLayout.ts`、`DesktopWindowFrame.tsx`、`DesktopTopOverlay.tsx`、`WorkspaceHeader.tsx`、`WorkspaceHeaderSections/WorkspaceHeaderActionSection.tsx`、`WorkspaceSidebar.tsx`、`WorkspaceSidebar/WorkspacePurposeSection.tsx`、`WorkspaceSidebarFooter.tsx`、`NewTaskButtonGroup.tsx`、`TaskList.tsx`、`TaskListItem.tsx`、`TaskRenameDialog.tsx`、`WorkspacePinnedTasksSection.tsx`、`WorkspaceTimelineTasksSection.tsx`、`lib/taskTimelineGroups.ts`、`lib/taskListItemPresentation.ts`、`SettingsPage.tsx`、`settings/SettingsPageParts.tsx`、`settings/settingsPageConfig.ts`、`settingsPageHelpers.tsx`、`components/ui/{button,dialog,toast,input,switch,select}.tsx`、`styles.css`、`useTheme.ts`。全部为本会话用 Read/rg 逐段读取，未运行构建或测试（本任务为只读调研，无验证命令要求）。

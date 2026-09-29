# KeenCode 设计系统

面向编码 Agent 的 UI 设计规范。在本仓库生成或修改任何界面前，先遵守本文件，不要自创视觉规则。

本文件的结构与规范写法参考 ZCode 设计规范（Apache-2.0，经授权作为视觉基线），全部令牌、规则与门禁以本仓库当前实现为准；实现与本文不一致时，以代码和门禁结果为准，并提请修正本文。主题与 Harness 来源细节见 `docs/frontend-harness-theme.md`。

## 最高优先级约束

以下三条由门禁强制执行（`pnpm run check:design-system`、`pnpm run lint:css`，均包含在 `pnpm test` 中），违反任何一条都视为设计系统缺陷，不是风格偏好：

1. **界面字号唯一输入是 `--ui-font-size`**
   - 界面文字尺寸只能消费 `--text-xs / --text-sm / --text-md / --text-lg / --text-xl`（由 `--ui-font-delta` 派生），或 Tailwind 侧唯一映射 `text-ui-base`（= `--text-md`）。
   - TSX 中禁止使用 Tailwind 内置字号类（`text-base`、`text-sm`、`text-xs`、`text-lg`）和任意值字号（`text-[13px]`）；更小字号在对应 `styles/app-*.css` 中用 `var(--text-*)` 表达。
   - 调整界面字号只更新 `--ui-font-size`，绝不修改 `html` 或 `document.documentElement.style.fontSize`；图标、间距、圆角等几何不随字号缩放。
   - 代码预览、终端等内容的独立字号设置是唯一的内容级例外，其周边控件、标签、头部仍走本刻度。
2. **颜色只用语义令牌**
   - 禁止 hex、rgba/hsl 字面量和 hue 类工具类（`bg-gray-100`、`text-slate-600`）；调色板按角色组织，不按色相组织。
   - 确需新颜色时，先在 `apps/ui/src/styles/tokens.css` 或 `ui-governance.css` 定义令牌，再消费令牌。
   - 禁止用中性 alpha 叠加（`text-white/60`、`border-black/10`）替代令牌。
3. **圆角只用 `var(--radius-*)`**
   - stylelint 规定 `border-radius` 仅允许 `var()`、`0`、`%`；不引入新的 px 圆角字面量，也不用语义不明的裸 `rounded`。

另有一条配套约束：业务代码不写 inline style，动态视觉值通过 CSS 自定义属性传递（design-system-gate DSG003）。

## 产品气质

KeenCode 是本地优先的桌面 AI 编码工具。界面应当沉静、紧凑、操作性强，而非装饰性。

设计目标：

- 长会话、高信息密度下的可读聊天与工具输出
- 键盘驱动的工作流
- Tauri 桌面（Windows、macOS、Linux）为验收目标；浏览器 dev server 只用于前端开发
- light / dark / system 三种主题偏好，默认 dark
- 国际化（zh、zh-TW、en），容忍文本长度变化

避免：

- 营销式大间距与松散留白
- 渐变作为默认 UI 语言
- 大面积品牌色整面填充
- 背景、卡片、浮层表面层级含混

## 令牌体系分层

令牌集中在 `apps/ui/src/styles/`，四层结构，依赖方向自上而下：

| 层 | 文件 | 内容与规则 |
| --- | --- | --- |
| Harness 底座 | `styles/harness/`（base、design-platform、gradient-shadow-text） | `--dsw-*`、`--ds-*` 平台变量，MIT 第三方固定提交。业务代码不直接引用，只经下层别名消费 |
| Appica 角色层 | `styles/ui-governance.css` | 无前缀 Appica 角色：`--foreground*`、`--background*`、`--border*`、`--primary*`、`--secondary*`、`--error/success/warning/info` 及其 `-subtle/-soft/-muted/-strong/-emphasis/-intense` 变体、`--focus-ring*`、`--tooltip*`，按 `[data-theme="light"]` 与 `[data-theme="dark"]` 双主题给出。对 Appica 组件是唯一权威 |
| 产品语义别名 | `styles/tokens.css` | `--bg-*`、`--text-*`、`--border-*`、主操作与强调色、几何/字体/动效/布局度量、功能色板。业务样式主要消费层 |
| 皮肤层 | `styles/theme-colors.css` | `html[data-base-color]`（gray/slate/zinc/neutral/stone）、`html[data-primary-color]`（base/品牌预设/custom）与 `html[data-secondary-color]`（品牌预设/custom）分别控制基础色、主色和次色；默认组合为 gray/base/blue，自定义主色和次色存为规范化六位 hex |

规则：

- 业务样式优先消费产品语义别名；在 Appica 组件定制上下文中使用 Appica 角色层；两层都不要下沉到 `--dsw-*`。
- `--primary` / `--primary-hover` / `--primary-fg`（主操作，黑白对比体系）与 `--accent` / `--accent-hover` / `--accent-muted` / `--accent-fg`（链接与蓝色强调）是两个语义，不可混用。
- 状态色 `--danger`、`--danger-muted`、`--success`、`--warning`、`--info` 只用于真实语义状态，不做装饰性增强。

### 产品语义别名速查

- 表面：`--bg-app`（应用底）、`--bg-main`（主工作区）、`--bg-sidebar`（侧栏）、`--bg-elevated`（抬升容器）、`--bg-card`（卡片）、`--bg-input`（输入域）、`--bg-code`（代码底）、`--bg-user-bubble`（用户消息气泡）、`--bg-overlay`（浮层）、`--bg-hover`、`--bg-active`
- 边框：`--border-subtle`（默认分隔）、`--border-popover`（浮层边框）、`--border-focus`（聚焦边框）
- 文字：`--text-primary`（主阅读文字）、`--text-secondary`（元数据与说明）、`--text-tertiary`（占位与弱提示）、`--text-caption`（说明性小字）、`--text-inverse`（深色/品牌/状态填充上的文字）
- 功能色板：`--change-*`（Git 变更视图）、`--kind-*`、`--dir-accent`、`--find-*`（页内搜索高亮）、`--analytics-model-1..8`（模型图表八色）、`--terminal-*`（xterm 运行时读取）、`--effort-*`

### 颜色使用规则

- 页面根用 `--bg-app` / `--bg-main`；侧栏用 `--bg-sidebar`；标准内容卡用 `--bg-card`；浮层用 `--bg-elevated` / `--bg-overlay` 配 `--border-popover`。
- 通用 hover 用 `--bg-hover`，激活/选中用 `--bg-active`。
- 优先用文字层级（`--text-primary` → `--text-secondary` → `--text-tertiary`）制造信息密度，而不是先加边框和颜色。
- 主按钮用 `--primary` + `--primary-fg`；链接与蓝色图标强调用 `--accent` 系。
- Git 变更、搜索高亮、终端、模型图表等场景使用各自专用色板，不借用通用状态色。
- 品牌/主色使用克制而有意图；不用品牌色做整页背景。
- 不要在没有明确分层理由时混用 `--bg-app`、`--bg-card`、`--bg-elevated`。

## 主题模式

- 用户可选 System / Light / Dark，默认 dark（新装即暗色体验）。
- 权威状态在 `@appica/ui-react` 的 ThemeProvider（`apps/ui/src/main.tsx`，storageKey `keencode.theme`）；dark 同时落在 `<html>` 的 `.dark` class 与 `data-theme` 属性上。
- 首绘前应用主题防闪：`index.html` 预置 `data-theme="dark"`，`main.tsx` 在渲染前同步。
- Tauri / macOS 原生窗口外观随主题同步（`apps/ui/src/lib/theme.ts` 的 `applyNativeWindowTheme`）。
- 皮肤偏好持久化在 localStorage（`keencode.base-color`、`keencode.primary-color`、`keencode.secondary-color`、`keencode.uiFontSize`）。
- 每个可见改动都必须在 light 与 dark 下验证；涉及主色/基础色选择的界面抽验不同皮肤组合。

## 字体排印

### 字族

- **UI Sans**：`--font-sans`（系统栈 + PingFang SC / Microsoft YaHei 中文 fallback），承载几乎所有界面文字；UI 默认字重 `--font-ui-weight: 430`。
- **UI Mono**：`--font-mono`（ui-monospace、SFMono-Regular、Menlo、Consolas + 中文 fallback），用于路径、命令、代码、标识符、快捷键、commit 哈希、模型 ID 和终端类数据。聊天作用域另有 `--chat-mono`（`lobe-chat.css` 内定义，同样映射应用字族）。

### UI 字号刻度

`--ui-font-size` 默认 `14px`（设置项范围 12–20px），`--ui-font-delta = --ui-font-size - 14px`：

| 令牌 | 公式 | 默认值 |
| --- | --- | ---: |
| `--text-xs` | `12px + delta` | 12px |
| `--text-sm` | `13px + delta` | 13px |
| `--text-md` | `14px + delta` | 14px |
| `--text-lg` | `16px + delta` | 16px |
| `--text-xl` | `28px + delta` | 28px |

- TSX 中使用 Tailwind 映射 `text-ui-base`（等于 `--text-md`，含行高）；其余档位在 CSS 中用 `var(--text-*)` 消费。
- 行高令牌：`--leading-tight 1.25`、`--leading-normal 1.5`。
- Markdown 正文与回合统计等内容字号跟随 `--dsh-content-font-size`，后者同样派生自 `--ui-font-size`。
- 小于 680px 视口下输入控件强制不小于 16px，防止 iOS 聚焦缩放（`ui-governance.css` 已全局处理）。

### 类型角色

按内容角色选择令牌，而不是按孤立视觉偏好：

| 令牌 | 主要角色 |
| --- | --- |
| `--text-xl` | 欢迎页、空态等一级阅读标题（28px 为庆典级，日常界面不用） |
| `--text-lg` | 二级标题、区块标题 |
| `--text-md` | 正文、常规按钮、工作区与分区标题等主要 UI 文字 |
| `--text-sm` | 次要说明、辅助信息、帮助文字、Markdown 行内代码 |
| `--text-xs` | 徽标、计数器、快捷键标签、极弱元数据 |

- 字号层级与颜色层级是独立决策：次要文字配 `--text-secondary`，弱元数据配 `--text-tertiary`。
- 标题与标签可以加重字重，但不为此改变语义字号档位。
- 尊重 i18n 膨胀：布局不得只对短中文/英文标签成立，不得把截断当作组件在翻译下存活的唯一手段。
- 等宽字体只用于内容本身技术性的地方。

## 间距

- 语义间距档 `--space-1..5` = 4 / 8 / 12 / 16 / 20px；其余用 Tailwind spacing 工具类，不另造间距令牌。
- 基础节奏是 4px；密集操作 UI 默认保持紧凑。
- flex 文本布局中需要截断或收缩的位置加 `min-w-0`；嵌套滚动或分栏中需要允许滚动的位置加 `min-h-0`。
- 内容容器优先 `w-full + max-w-*`（阅读列宽 `--content-max-width: 920px`）；固定宽度只用于菜单、浮层、对话框和稳定侧栏。

## 圆角

阶梯令牌：`--radius-2xs 2.5px`、`--radius-xs 5px`、`--radius-sm 7.5px`、`--radius-md 10px`、`--radius-lg 12.5px`、`--radius-full`。

专用令牌：`--radius-window` / `--radius-composer`（16px）、`--menu-radius`（20px）、`--modal-radius`（15px）、`--radius-settings`（30px）、`--radius-stat`（28px）、平台工作区 `--radius-workspace-win/mac/other`。

规则：

- 圆角一律来自 `var(--radius-*)`；需要新专用圆角时先在 `tokens.css` 定义令牌并注明用途。
- 嵌套的可见圆角容器就近降一档；同层同级容器使用同档圆角；布局区域、普通包装层和分隔线不计入圆角层级。
- `--radius-full` 只用于明确的胶囊形或圆形，按钮、标签、计数器、图标按钮不因组件类型自动获得。
- 拼接为连续形状的相邻表面可以用 `0` 或单侧清除圆角。

## 尺寸

- 布局度量令牌：`--sidebar-width 264px`、`--aside-width 360px`、`--content-max-width 920px`、`--titlebar-height 48px`、`--control-height 36px`。
- 触控目标（移动端）：`--ui-touch-target` 32 / 44 / 56px。
- 控件高度优先复用 Appica 尺寸变体；Appica 组件的 `size` 必须显式传值且使用官方变体，默认 `md`（DSG005）。
- 避免为普通业务 UI 引入任意 `w-[...]`、`h-[...]`。

## 阴影与层级

- 克制用影：`--shadow-composer`（输入区）、`--shadow-composer-context`、`--glass-shadow`（玻璃浮层）、`--shadow-pop`（弹出强调）。
- 优先用背景对比、边框和圆角分层，其次才靠阴影；浮层（菜单、弹窗）可用 `--shadow-pop`，但仍保持紧凑可控。
- 背景分层通常比阴影强度更重要。

## 动效

- 时长令牌：`--motion-fast 150ms`、`--motion-enter 200ms`；缓动 `--ease-out`、`--ease-in-out`。
- 动效快速、低调，用于澄清状态变化，不装饰屏幕；避免长、弹跳、玩闹的动画出现在主工作区。
- `prefers-reduced-motion` 下全局过渡归零；新增动效不得绕过该兜底。

## 组件

### 组件来源与门禁

- 可见交互控件一律来自 `@appica/ui-react`（子路径逐组件 import，如 `import { Button } from '@appica/ui-react/button'`）或 `apps/ui/src/components/ui/` 下的产品化包装；业务代码禁止新增原生 `button`、`input`、`textarea`、`select`、`dialog`（DSG001）。
- 豁免仅限：浏览器要求的隐藏控件（如 `components/host/MobileRemoteShell.tsx` 的隐藏 file input）、`contenteditable` 与媒体等无等价组件的宿主；豁免必须在代码注释中说明原因，不另建可见控件样式。
- `components/ui/` 包装层的职责是锁定产品约定（如 `card.tsx` 锁定 `border-border bg-card text-foreground` 表面、`textarea.tsx` 强制 `inputSize="md"` + `text-ui-base`）；新增包装前先确认没有等价物，缺失时先查 Appica 文档再组合。
- 图标只用 Tabler Icons，统一经 `components/icons.tsx` 的 `Icon*` 再导出。
- 合并类名用 `cn()`（`apps/ui/src/lib/utils.ts`，clsx + tailwind-merge）。

### 按钮规则

- 复用 Appica Button 既有变体与尺寸；icon-only 按钮保持方形。
- 不把每个操作都升为主操作；每个面板内保持清晰的动作层级。
- React 19 下 `ref` 是普通 prop，不写 `forwardRef`。

### 输入

- 默认 `--bg-input` 表面、`--border-subtle` 边框；聚焦用 `--border-focus` 与 `--focus-ring*`。
- 输入应安静集成，不默认发光；错误态只用于真实校验问题；不把普通输入样式做成卡片。
- 复合输入外壳（如主输入区）按容器层级处理，表面与阴影走专用令牌。

### 菜单、浮层、对话框

- 浮层表面用 `--bg-elevated` / `--bg-overlay`，配 `--border-popover` 与 `--shadow-pop`。
- 菜单行紧凑、高可扫读：行 hover 用 `--bg-hover`，不做整行边框或强填充；选中项优先勾选/单选指示，不做强整行选中底色。
- 禁止把菜单做成卡片，也不用 `--bg-card` 充当普通菜单内容。
- 禁用项保持布局与层级，通常只降文字颜色。
- 浮层精确紧凑，避免松散的大 popover；浮层与触发器保持小而一致的偏移。

### 标签页与选中态

- 未激活标签保持中性；激活用更强的表面对比，不做品牌色填充块。
- 列表行或标签的选中用 `--bg-active`，hover 用 `--bg-hover`。

## 聊天与开发者 UI

会话渲染主区在 `apps/ui/src/components/lobe-chat/`（`ConversationThread.tsx` 拥有消息渲染、活动时间线与滚动）：

- 用户消息用气泡底 `--chat-bubble`（映射 `--bg-user-bubble`）；助手消息与工具时间线不用气泡底。
- Markdown 栈为 streamdown + CJK 插件 + KaTeX + mermaid + Shiki；聊天作用域视觉变量集中在 `lobe-chat/lobe-chat.css`，全部映射回应用令牌（`--chat-text` → `--text-primary`、`--chat-code-bg` → `--bg-code` 等）。禁止在聊天作用域引入独立色值或第二套字号体系。
- 代码块行级高亮用 Shiki，默认不显示文件名与行号；代码、命令、路径、哈希一律等宽。
- 工具步骤按流序内联渲染；失败用安静的红标（`--danger` 系），不弹强警告块。
- Git 变更视图用 `--change-*` 专用色板；xterm 终端配色运行时读取 `--terminal-*`。
- 密集操作面板优先于营销卡片式排版。

## 布局与响应式

- 壳层是 `App.tsx` 的 `.app-shell.platform-* > .workbench`，sidebar / main / aside 三栏；结构类名样式集中在 `styles/app-*.css`，采用 BEM 风格（如 `.composer__row`）。布局框架不计入内容圆角层级。
- 小于 760px 视口侧栏抽屉化、单列呈现；核心操作在所有断点保留。
- 断点只用于布局、宽度、可见度和密度变化，不改变组件语义；优先改 `max-width`、`grid`、`flex` 和可见性，而不是更换组件形态。

## 可访问性与国际化

- 键盘导航是一等交互路径；焦点样式走 `--focus-ring*` 既有模式，不新造焦点表现。
- 状态表达配合可读文字，不只靠颜色区分。
- light 与 dark 下对比度保持安全；forced-colors 适配有全局兜底，新控件不得绕过。
- 文案经 `apps/ui/src/i18n`（zh、zh-TW、en，en 为键权威）；删除调用点时同步删除三种语言的键。
- 有文字标签更实用时，避免纯图标表达含义。

## 实施指引

- 先复用语义令牌与既有组件，再考虑新增。
- 出现新 UI 需求时，先判断它属于结构、表面、交互还是状态，再据此选令牌。
- 样式改动必须跑 `pnpm run lint:css` 与 `pnpm run check:design-system`；Appica 新组件使用前先取 `https://appica.dev/llms.txt` 索引并读 `<url>.md` 文档。
- 拿不准时，选择更安静的 UI 和更强的信息层级。

## Do

- 一致使用语义颜色令牌，保持页面背景、卡片、浮层三种表面的区别
- 控件紧凑、操作性强，用文字层级表达密度
- 技术值与命令类内容用等宽
- light 与 dark 双主题验证，兼顾三个桌面平台
- 考虑本地化与长标签

## Don't

- 在普通 UI 中使用裸色值、hue 类或 alpha 叠加
- 用品牌色填充大面积表面
- 引入没有稳定系统理由的圆角、阴影、宽高
- 把语义色当装饰，或让菜单、对话框在应密集时变得松散
- 制造只在一个主题或一个皮肤组合下正确的组件
- 在工具密集的屏幕上为视觉新奇牺牲清晰度

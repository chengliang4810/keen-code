# KeenCode Go 迁移第一版方案（Go + mygo 原生 UI，界面按 ZCode 前端复刻）

> 目标：第一版交付**完整可每日使用**的桌面产品：Go 实现 Agent 运行时与供应商适配，mygo 原生 UI（无 webview）实现界面，视觉按 ZCode 前端规格复刻。
> 依据：仓库根 `AGENTS.md`（产品语义必须继承）、`docs/go-migration/` 下四份规格文档、mygo 源码（`/Users/chengliang/code-projects/mygo`，MIT 许可，module `github.com/egoist/mygo`，go 1.27.1，`go.mod:3`）与旧栈 Rust 源码对照。所有 mygo/旧栈结论均给出 `文件:行` 引用；本会话实际执行过的验证见 §11。

---

## 0. 结论与关键决策

| # | 决策 | 依据 |
| --- | --- | --- |
| D1 | **可行动手**：mygo `ui` 包支撑聊天类桌面应用可行（flexbox/grid、段落虚拟化、富文本混排、IME、无窗口测试、亮暗主题、SVG 图标齐备）；需要自建的只有 Markdown 解析层 | `docs/go-migration/mygo-ui-notes.md` §5、§10；本会话复核 `ui/richtext.go:15-33`、`ui/list.go:38-49`、`ui/editor.go:718-721`、`ui/handler.go:40-59` |
| D2 | **`mygo.Bind` 不用于第一版**。`Bind`（`ipc.go:71-96`）是把 Go 服务暴露给 **webview JS 前端**的 IPC 机制（生成 TS 客户端）；`ui.View` 原生窗口没有 JS 页面（`ui/window.go:18-31`、`window.go:200-206`：有 Content 的窗口忽略 URL/Page）。第一版服务层按 **Bind 兼容形状**设计（导出方法、JSON 可编码参数与返回值、`error` 结尾），main 装配后进程内直调；未来做 web 表面时可直接 `mygo.Bind` 复用 | 本会话读 `ipc.go:90-104`、`ui/window.go:18-31` |
| D3 | **主题只做 zai 两面**（zai-light / zai-dark，值照抄 `zcode-tokens.md` §2），默认 zai-dark；默认主题 neutral 面推迟。亮暗跟随系统 + 设置项固定 | 产品默认皮肤即 zai（`zcode-shell-specs.md` §5、`zcode-tokens.md` §0） |
| D4 | **全新数据命名空间 `~/.keencode/go-v1/`**，与旧 Tauri 栈共存互不干扰；不迁移旧数据、不做旧字段回退 | `AGENTS.md`（"按全新项目维护唯一配置和数据结构"）；旧栈根目录为 `~/.keencode`（`apps/desktop/src/storage.rs:17-36`） |
| D5 | **Go 模块放仓库根**：根目录新增 `go.mod`（`module keencode`），与 `package.json`/`Cargo.toml` 共存；`apps/`、`core/`、`tooling/` 旧栈整目录只读作参考 | ask 指定 module `keencode` + `cmd/keencode` |
| D6 | **API Key 明文存本地 config（0600）**；系统 keyring 推迟 | 第一版最小闭环；旧栈仅插件密钥用 keyring（`apps/desktop/src/plugin_secrets.rs`） |
| D7 | **工具权限最小闭环**：工具按 `Effect` 分类，只读自动放行；副作用（Write/Edit/Bash）每次弹原生对话框确认（允许/拒绝）。"总是允许"白名单推迟 | ask §5；`mygo.Dialog.Message`（`dialog.go:181-228`，gallery 实证可从 UI goroutine 调用：`examples/gallery/main.go:1447-1448`） |

---

## 1. 架构总览

### 1.1 模块布局

```
<仓库根>/
├── go.mod                      # module keencode；go 1.27.1；require github.com/egoist/mygo（replace 一律不进 go.mod，本地开发经 go.work 指向 mygo 检出，见风险 8）
├── cmd/keencode/main.go        # 装配入口：config → runtime → services → 窗口
├── internal/
│   ├── model/                  # Provider 中立消息、流事件、请求/响应、错误
│   │   ├── message.go          #   Role / Message / ContentBlock（Text/Reasoning/ToolCall/ToolResult）
│   │   ├── request.go          #   ModelRequest / ToolDefinition / ToolChoice / ReasoningConfig
│   │   ├── stream.go           #   StreamEvent / StopReason / TokenUsage
│   │   ├── response.go         #   ModelResponse / ResponseMetadata
│   │   └── errors.go           #   ModelError（校验/中断/部分文本）
│   ├── provider/
│   │   ├── provider.go         #   Provider 接口、Complete 收集器、New 工厂
│   │   ├── anthropic/          #   Messages 协议：adapter.go / wire.go / stream.go（SSE→StreamEvent）
│   │   └── openai/             #   Chat Completions 协议：adapter.go / wire.go / stream.go
│   ├── tools/
│   │   ├── tool.go             #   Tool 接口 / Invocation / ToolOutput / Effect
│   │   ├── registry.go         #   Registry（名称唯一、fail-closed）
│   │   ├── fs.go               #   Read / Write / Edit
│   │   ├── search.go           #   Glob / Grep
│   │   └── shell.go            #   Bash（进程组、超时、取消、输出上限）
│   ├── agent/
│   │   ├── loop.go             #   Agent Loop（模型流→工具→回填，循环至终态）
│   │   ├── event.go            #   Event 信封与种类（唯一时间线事件）
│   │   ├── turn.go             #   TurnRequest / 取消
│   │   └── prompt.go           #   系统提示词组装（v1 固定模板，产物放 ~/.keencode 沙箱外无需）
│   ├── runtime/
│   │   ├── manager.go          #   SessionManager：创建/列表/重命名/删除/打开恢复
│   │   ├── session.go          #   Session：Send / Stop / Subscribe / History
│   │   ├── journal.go          #   JSONL 追加日志 + 顺序重放 + 半行容错
│   │   ├── draft.go            #   草稿持久化：会话草稿 draft.txt / 新会话草稿 draft-new.json
│   │   └── hub.go              #   每会话事件广播（订阅/退订，回放与直播同锁无缝衔接）
│   ├── config/
│   │   ├── config.go           #   Config / ProviderConfig / ModelRef / ThemeSetting / ResolveURL
│   │   ├── store.go            #   读写（临时文件+rename 原子写，0600）
│   │   └── paths.go            #   ~/.keencode/go-v1 布局
│   ├── ui/
│   │   ├── theme/
│   │   │   ├── palette.go      #   Palette 结构 + zaiLight/zaiDark 两套常量（§3）
│   │   │   ├── bridge.go       #   Palette → mygo ui.Theme 覆盖 + theme.Active(c)
│   │   │   └── scale.go        #   字号/字重/圆角/间距常量
│   │   └── kit/                #   组件库，三人并行（§4 kitPartition）
│   │       ├── kit.go          #   共享类型契约（A 写，W0 冻结；含 PromptOptions）
│   │       ├── icon.go button.go input.go editor.go checkbox.go badge.go spinner.go        # A
│   │       ├── menu.go select.go popover.go tooltip.go dialog.go toast.go                  # B
│   │       ├── reasoning.go message.go chatlist.go  # C：MujicaUI chat/agent 组件的接入层（§4.2）
│   └── app/
│       ├── app.go              #   应用状态根（窗口引用、Router、服务指针、合帧泵）
│       ├── services.go         #   SessionService / SettingsService / ProjectService（§5.7）
│       ├── permission.go       #   Authorize 桥：副作用工具 → 原生对话框
│       ├── shell.go            #   外壳：侧栏（会话列表/新建）+ 分隔条 + 主区 + 顶栏
│       ├── chat.go             #   聊天视图：时间线 + composer + 错误横幅 + 停止
│       └── settings.go         #   设置页：供应商与模型管理、主题
└── apps/ core/ tooling/        # 旧栈：只读参考，不改动不引用
```

### 1.2 依赖方向（禁止环）

```
cmd/keencode → app → {runtime, config, agent, provider, tools, model, ui/kit}
ui/kit → ui/theme → mygo/ui        （kit 仅依赖 theme、mygo/ui 与自身子包）
runtime → agent → {provider, tools, model}
provider/tools → model             （model 不依赖任何 internal 包）
```

- `model`、`ui/theme`、`ui/kit` 是三块无外部 IO 的纯库，最先冻结；kit 依赖 `ui/theme`、`mygo/ui`，聊天/Agent 内容渲染直接使用 MujicaUI（`github.com/ZacharyZhang-NY/MujicaUI`，MIT；agent/chat 包）的官方组件，每帧先 `core.Use` 再 `theme.Apply` 覆盖 zai 调色板（两套令牌互不干扰：MujicaUI 从 Use 安装的窗口本地设置取色，kit 从 c.Theme 取色）。
- Agent Loop、工具运行时、Session、Provider 保持 Provider 中立：不出现 "anthropic"/"openai" 字样于 `internal/agent`、`internal/runtime`（协议名只存在于 `internal/provider/{anthropic,openai}` 与 `internal/config` 的 Protocol 字段）。
- UI 不实现模型协议与工具调度；文件系统/进程/对话框能力全部在 Go 侧（对齐 `AGENTS.md` "Rust 系统能力边界"条款的 Go 等价物）。

### 1.3 线程模型（mygo 硬约束）

- main goroutine：`mygo.App.Run()` + 每帧 view 构建（`runtime.LockOSThread` 在包初始化完成，`mygo.go:72-76`）。
- Turn goroutine：每会话至多一个，跑 Agent Loop。
- 事件路径：`Session.Subscribe()` channel → app 的合帧泵（16ms 窗口批量 drain）→ `win.Update(fn)`（`content.go:148`）改状态并触发重绘。**任何 goroutine 不直接写 UI 状态**。
- 原生对话框是阻塞 API：`Authorize` 从 Turn goroutine 调 `mygo.Dialog.Message`，mygo 内部转发主线程并等待（`mygo.go:44-49`；`docs/native.md:9-13`；gallery 同款调用 `examples/gallery/main.go:1447-1448`）。
- 空闲窗口不渲染，2 秒后释放 CPU 帧（`docs/ui/rendering.md:48-50`）——符合"无任务不轮询"。

### 1.4 旧栈处理

`apps/`、`core/`、`tooling/` 保留只读：迁移期间 Tauri 版仍可运行（数据隔离见 D4）；Go 版遇到语义问题时按 `AGENTS.md` 模块表回查旧栈源码（如 `#` 语义在 `apps/desktop/src/providers.rs:1298-1340`），**不 import、不共享构建产物**。

---

## 2. 架构图（数据流）

```
┌─ main goroutine ──────────────────────────────┐
│ mygo.App.Run                                  │
│  └─ Window(Content: ui.View(app.view))        │
│      每帧: app.view(c)                        │
│        ├─ shell: 侧栏(会话列表 ui.List)        │
│        ├─ chat:  kit.ChatList → MujicaUI       │
│        │          chat.MessageList(Key/Date)   │
│        ├─ composer: kit.Editor(Enter→Send)    │
│        └─ settings: kit.Select/Form           │
└───────────────▲───────────────────────────────┘
                │ win.Update(合帧 16ms)
┌───────────────┴───────────────┐   channel
│ app 泵 goroutine: drain订阅 chan │◄──── runtime.Session(hub)
└───────────────────────────────┘
                │
┌───────────────▼───────────────┐
│ Turn goroutine: agent.Loop     │
│  provider.Stream(HTTP/SSE)    │
│  tools.Registry.Execute       │
│  ├─ 只读 → 直接执行             │
│  └─ 副作用 → Authorize 回调     │──► 主线程: Dialog.Message(允许/拒绝)
│  journal.Append(每个 Event)    │──► ~/.keencode/go-v1/sessions/<id>/journal.jsonl
└───────────────────────────────┘
```

---

## 3. ZCode 令牌 → Go 常量映射规则（`internal/ui/theme`）

### 3.1 映射规则

1. **一个 Go 字段对应一个 CSS 令牌**：`Palette` 结构体字段名 = CSS 变量名去 `--color-` 前缀的 CamelCase（`ForegroundSubtle` ↔ `--color-foreground-subtle`），字段注释写 CSS 名与 `zcode-tokens.md` 节号，方便 diff 回源。
2. **值照抄**：字面 hex 用 `ui.Hex("#161616")`（`ui/color.go:24-30` 支持 `#rrggbb`，解析失败 panic —— 在包级 `init` 常量表中暴露，等价编译期校验）。
3. **透明混色换算**：`color-mix(in oklab, X n%, transparent)` ≈ 同色 alpha=n% → `ui.RGBA(r,g,b,0.10)`（如 `Border` = `#0A0A0A@10%`）。
4. **预合成**：会叠在已知背景上的半透明令牌（surface、selected、hover）在构建 Palette 时用 `Color.Over(base)` 预合成不透明值（`ui/color.go:103`），因为 mygo 控件不叠多层背景；需要真透明的（对话框遮罩 black/60）保留 alpha。
5. **两套常量值**：`var zaiLight, zaiDark Palette`，全部取自 `zcode-tokens.md` §2 表；产品默认 **zaiDark**。
6. **每帧选择**：`theme.Active(c)` 按 `c.Theme().Dark`（mygo 随系统外观自动换主题并重绘，`ui/context.go:82-103`）返回 zaiLight/zaiDark 对应面；用户在设置里固定 light/dark 时，启动时映射为 `mygo.Theme.SetSource(ThemeLight|ThemeDark|ThemeSystem)`（`modules.go:214-216`）。
7. **桥接内建控件**：每帧 `bridge.Apply(c, pal)` 把 Palette 映射进 `ui.Theme` 字段后 `c.SetTheme(&t)`（`ui/context.go:205-209`），使 mygo 原生控件免费获得 ZCode 颜色。映射表（字段名以 `ui/theme.go:7-51` 为准，**`Scrollbar` 是 Color、`ScrollbarWidth float32` 是平级字段**，`theme.go:36-40`——没有 `Scrollbar.Width`）：`Background←background、Surface←surface(合成)、SurfaceHover←surface-hover(合成)、SurfacePressed←selected(合成)、Border、Text←foreground、TextMuted←foreground-subtle、Accent←brand、AccentHover←hover(合成)、AccentPressed←selected(合成)、**AccentText←primary-foreground**、Danger←destructive、Warning、Success、Selection←selected(合成)、Focus←input-border-focused、Scrollbar←border-hover(合成)、**ScrollbarWidth=14**、Radius=8(lg)、Spacing=4、FontSize=14`。**警示**：zai-dark 的 Accent=brand=`#ffffff`，若漏映射 `AccentText`/`Focus`，开关/复选/PrimaryButton/焦点环/Select 高亮会白底白字（mygo DarkTheme 默认 AccentText=`#ffffff`，`theme.go:129`）。
8. **禁则**（对齐 `zcode-chat-specs.md` §6.6）：kit 与 app 中禁止 `ui.Hex` 字面量、禁止裸 px 字号、禁止直接引用 `c.Theme()` 原生字段；一律 `theme.Active(c)` 字段 + `scale` 常量。

### 3.2 字号 / 圆角 / 间距（`scale.go`，值出自 `zcode-tokens.md` §5-§7）

```go
// 字号（--ui-font-size 基准 14）
const (FontXL=18; FontLG=16; FontBase=14; FontCaption=13; FontSM=12; FontXS=10)
const LineHeightBody = 1.75           // 助手正文行高（message.tsx:1367-1370 leading-[1.75]，zcode-chat-specs §1.3；
                                      // 1.625 是 Tailwind relaxed，第一版不使用）
const (WeightNormal=400; WeightMedium=500; WeightSemibold=600; WeightBold=700)
// 圆角（DIP）
const (RadiusXS=2; RadiusSM=4; RadiusMD=6; RadiusLG=8; RadiusXL=12; RadiusXXL=16; RadiusFull=9999)
// 间距节奏（zcode-chat-specs §6.1：轮间 pt-14/pb-5、轮内 gap-5、工作项 gap-4）
const (SpaceUnit=4; SpaceTurnTop=56; SpaceTurnBottom=20; SpaceInTurn=20; SpaceWorkItem=16)
// 消息列宽（zcode-chat-specs §1.1 conversationLayout 断点 864/1280 容器查询的 v1 近似）
const (ColumnDraft=672; ColumnSession=896; ColumnSessionWide=1152)
```

等宽字体走 `.Font("monospace")`（系统等宽栈，`ui/element.go:830-834`）；正文走系统默认字体栈——ZCode `font-sans` 本就是系统栈（`zcode-tokens.md` §7.3）。

### 3.3 Palette 字段清单（第一版用到的全部令牌）

背景层：`Background WinAlt BackgroundAlt Header Panel Sidebar Surface SurfaceHover Card CardSelected Popover PopoverHeader Input InputFocused Tab TabActive Menu MenuHover Toast Tooltip TooltipTag`
前景层：`Foreground ForegroundSubtle ForegroundSubtlest ForegroundInverse Primary PrimaryForeground Secondary TooltipForeground TooltipTagForeground Tag`
边框：`Border BorderHover InputBorderHover InputBorderFocused`
强调/状态：`Brand Accent Hover Selected IconBlue Success Warning Destructive DiffAdded DiffRemoved FindHighlight FindHighlightActive`
轨迹 5 色：`TrajectoryUser TrajectoryAssistant TrajectoryReasoning TrajectoryToolCall TrajectoryToolResult`

> `IconBlue`（`--color-icon-blue` = `var(--color-terminal-bright-blue)`，ZCode `styles.css:162`，zai 未重定义 icon-blue 本身、随 terminal 变量解析）：**zai 亮 `#0066dd`（styles.css:518）/ zai 暗 `#80beff`（:663）**；默认主题亮 sky-500 `#00A6F4`（:217）/ 暗 sky-600 `#0084D1`（:369）仅备查——D3 决定 zai 两面为主，故取前者。
（图表色板、workflow 色列推迟；第一版不画图表/工作流。）

---

## 4. 组件库 kitPartition（`internal/ui/kit`，三人并行）

### 4.1 共享契约文件 `kit/kit.go`（实现者 A 起笔，W0 冻结；B/C 按本节签名先行开工）

```go
package kit

import (
    "github.com/egoist/mygo/ui"
    "keencode/internal/ui/theme"
)

// Variant 控件语义变体（ZCode button variants，zcode-shell-specs §4.1）。
type Variant string
const (
    VariantPrimary     Variant = "primary"     // bg-primary 文字 primary-foreground（zai 下黑白反转）
    VariantSecondary   Variant = "secondary"   // bg-secondary
    VariantGhost       Variant = "ghost"       // 无底色，hover bg-surface-hover
    VariantOutline     Variant = "outline"     // border + 透明底
    VariantDestructive Variant = "destructive" // bg-destructive 白字
)

// Size 尺寸档位。
type Size string
const (
    SizeSM     Size = "sm"      // h-8（lg 档 h-8 的按钮行）
    SizeMD     Size = "md"      // 默认
    SizeLG     Size = "lg"      // h-10（对话框主按钮）
    SizeIconSM Size = "icon-sm" // 24px 方形
    SizeIconMD Size = "icon-md" // 28px 方形（composer 发送/停止）
)

// IconName 图标清单：A 在 icon.go 实现 SVG 注册表；新增图标先在此声明再实现。
type IconName string
const (
    IconArrowUp IconName = "arrow-up"       // 发送
    IconArrowDown IconName = "arrow-down"   // 回到底部
    IconSquare IconName = "square"          // 停止
    IconBrain IconName = "brain"            // 思考
    IconChevronRight IconName = "chevron-right"
    IconChevronDown IconName = "chevron-down"
    IconCopy IconName = "copy"
    IconCheck IconName = "check"
    IconPlus IconName = "plus"              // 新建会话
    IconGear IconName = "gear"              // 设置
    IconInfo IconName = "info"              // 错误横幅
    IconAlert IconName = "triangle-alert"   // warning toast
    IconTerminal IconName = "terminal"      // Bash 工具卡
    IconFile IconName = "file"
    IconPencil IconName = "pencil"          // 重命名
    IconTrash IconName = "trash"            // 删除
    IconX IconName = "x"                    // 关闭
)

// P 返回当前帧的 ZCode 调色板（kit 内唯一取色入口）。
func P(c *ui.Context) *theme.Palette { return theme.Active(c) }

// PromptOptions 重命名/输入对话框契约（类型在 kit.go 冻结，B 在 dialog.go 实现）。
type PromptOptions struct {
    Title, Description           string
    Placeholder, Initial         string
    ConfirmLabel, CancelLabel    string
}
```

### 4.2 文件划分与相互依赖

| 实现者 | 文件 | 内容与规格出处 | 可依赖 |
| --- | --- | --- | --- |
| **A 基础控件** | `kit.go`、`icon.go`、`button.go`、`input.go`、`editor.go`、`checkbox.go`、`badge.go`、`spinner.go` | 见下 | theme |
| **B 浮层** | `menu.go`、`select.go`、`popover.go`、`tooltip.go`、`dialog.go`、`toast.go` | 见下 | A 全部、theme |
| **C 内容渲染** | `reasoning.go`、`message.go`、`chatlist.go`（MujicaUI `chat`/`agent` 组件的接入层） | 见下 | A 全部、B（仅 tooltip/dialog）、theme、MujicaUI |

A 不依赖 B/C；B、C 可依赖 A；**B/C 不得修改 kit.go 与他人文件**，缺图标/变体先在 PR 评论提出、由 A 补契约。跨界组件归属：错误横幅、消息条目归 C；确认对话框归 B。

**A — 基础控件**

- `icon.go`：`func Icon(c *ui.Context, name IconName, sizeDIP float32) *ui.Element`。包级 `map[IconName]*ui.SVG`（`ui.MustParseSVG` + `//go:embed svg/*.svg`，`ui/svg.go:51,81`），随 `TextColor` 着色。SVG 来源：ZCode 图标为 lucide 系（ISC），按需内嵌所需 17 枚。
- `button.go`：`func Button(c *ui.Context, v Variant, s Size, label string) *ui.Element`（`.Clicked()` 判定）、`func IconButton(c, v Variant, s Size, name IconName) *ui.Element`、`func SendButton(c, canSend bool, running bool) *ui.Element`——**状态机**：非 running → 发送钮（bg-brand 圆角 lg、图标 ArrowUp，`canSend=false` 时禁用并挂原生 `.Tooltip()` 字符串提示——A 流只用原生 Tooltip，不依赖 B 的 tooltip.go）；running → 一律显示停止钮（secondary + Square fill，Esc 触发）。与 ZCode「运行中且有草稿仍显示发送（入队语义），`showStopControl = canStop && !hasDraftToSubmit`，zcode-chat-specs §2.2」不同：v1 无队列，运行中草稿保留、发送禁用，见 §10 差异 8。选中态、禁用态按 variant 表着色，全部取 palette。
- `input.go`：`func TextField(c *ui.Context, value *string, placeholder string) *ui.Element`——单行 **`ui.TextInputBase`**（`base.go:413-422` 无外观底座；**禁用 `ui.TextInput`**，理由同 `editor.go`：自带主题底/边框会双层）自绘 ZCode 规格：`h-7 rounded-md px-2 border bg-input`，hover 边框 `InputBorderHover`，focus 边框 `InputBorderFocused`（zcode-shell-specs §3 控件规格）。
- `editor.go`：**多行输入壳**（聊天 composer 核心）。`func Editor(c *ui.Context, draft *string, opts EditorOptions) EditorResult`。
  - 壳：`rounded-2xl border border-input-border bg-input p-3`；focus 时边框取 `InputBorderFocused` 字段 + `bg-input-focused`（注意：zai 两面中 `--color-input-border-focused` = border-hover 色而非 brand，`zcode-tokens.md` §2；与 `input.go` 同一取色，`ChatPromptEditor.tsx:346-351`，见 zcode-chat-specs §2.1）。
  - 内芯：**`ui.TextAreaBase`**（`base.go:413-422`，无外观底座）。**禁用 `ui.TextArea`**——它构造时即画 `Background(t.Surface).Border(1, t.Border)`+主题圆角（`editor.go:726-733`），与 kit 壳叠成双层底/双边框，首帧像素对照必炸。
  - **Enter 发送（帧时缓冲差分，唯一在公开 API 内成立的设计）**：
    - `HandleInput`（`handler.go:73`）里：非 `InputKeyDown+KeyEnter` 直接放行；`ev.Mods&ui.Shift != 0` 放行（Shift+Enter 换行；**`ui.Modifiers` 是 uint8 位掩码，修饰键是包级常量 `ui.Shift/Ctrl/Alt/Super`（`keys.go:99-110`），没有 `.Shift` 字段**）；其余 Enter：记 `enterSeen=true` 与按 Enter 前的快照 `snapshot=*draft`，**return false 放行**给内建编辑器。
    - 视图函数里（TextAreaBase 构造之后——编辑器队列已在建帧时应用，此时读 `*draft` 是新值）：若 `enterSeen`，清位后判定 `*draft == snapshot+"\n"` 成立则 `EditorResult.Submitted=true`（app 层据此 Send 并清空草稿）；否则（提交组合正文等）不发，并剥离该次 Enter 产生的单个尾部 `"\n"`。
    - **为什么必须这样做（两级实证）**：macOS `keyDown:` 先无条件上报 KeyPressed、之后才 `interpretKeyEvents:` 交输入法（`internal/darwin/surface.go:701-709`），ui 引擎把 KeyPressed 无组合态守卫地投给 fn（`ui/input.go:53 → 487-495`）——组合态用于上屏的回车也会先以 `InputKeyDown+KeyEnter` 形态到达；而**带内建编辑器的元素（TextAreaBase 即是），`InputCompose/InputText` 根本不投给 HandleInput**（`input.go:688-694`：仅 `h.editor==nil` 才投 fn，否则直入内建编辑器队列），故「fn 内自维护组合态」同样不可行。本会话 headless 实测：朴素「拦 Enter」谓词 `sent=1`（复现误发送）；组合态跟踪谓词同样 `sent=1`（InputCompose 未到 fn）；帧时差分方案三用例全过（见 §4.3）。真实平台安全性由队列顺序保证：上屏时 insertText 先于 editKey 到达编辑器（`internal/darwin/surface_input.go:126-135`），缓冲差分必不等于单个 `"\n"`。
    - 已知边界（记 §10 差异 11 与风险 4）：headless 的 `tt.Compose` 后直接 `Key(Enter)` 会丢弃 marked 文本再换行（`editor.go:688-691` `commitCompose`），与真机 insertText 先行不同——headless 用例须以 `Compose+Type` 建模提交（`headless.go:354-361`）；真机 IME 行为在 W4 原生验收覆盖，并建议向上游提 composition 查询 API 的 issue。
  - 出口**只留帧后判定**：`EditorResult{ Submitted bool }`；不提供 OnSend 回调（单一语义，帧后判定更贴 mygo 事件习惯）。
  - `EditorOptions`：Placeholder / Disabled；占位符不是 Text 元素（notes §9.5），`.Label("Draft")` 供测试定位。
- `checkbox.go`：`Checkbox(c, *bool, label)`、`Switch(c, *bool)`（设置页，`ui/widgets.go:237-317`、`feedback.go:24` 包装 ZCode 配色：开 `bg-primary`、关 `primary/30`）。
- `badge.go`：`Badge(c, text, style)`（pill：`bg-surface rounded-md px-2.5 py-1 text-ui-base font-medium`，zcode-shell-specs §3）与 `StatusDot(c, color)`（6px 圆点：运行中换 `Spinner`）。
- `spinner.go`：`Spinner(c, size)`（ChatLoading 自旋点 `size-4/size-6`，`chat-loading.tsx:21-36`）。原 ShimmerText 扫光近似已随聊天组件改用 MujicaUI 官方动效移除。

**B — 浮层**

- `menu.go`：**接受系统原生菜单**。`ContextMenu` 的 build 收 `*ui.Menu`，项是 `m.Item(label string) *MenuItem` + `.Chosen()`（`menu.go:129-131`），由系统原生渲染（`menu.go:10-13` "The system shows it"）——**不能向菜单放 `ui.Element`，也没有逐项样式钩子**（rounded-md/hover 色/键位右对齐均做不到）。本文件只做薄封装：`func ContextMenu(c *ui.Context, build func(m *ui.Menu))` 与会话行菜单构造器（重命名/删除项 + Chosen 分派）；原生外观差异记 §10 差异 10。自绘菜单（Popover 版，可完全自定义样式）推迟 V1.1。
- `select.go`：`func Select[T ~string](c *ui.Context, sel *T, items []SelectItem[T]) *ui.Element`——`ui.SelectBase`（`base.go:254-358`）自绘：trigger `h-7 rounded-md pl-2 pr-1 border bg-input`（lg 档 `h-8 rounded-lg pl-3 pr-2`），弹层 `rounded-lg border bg-menu p-1 shadow-md`、项再 `rounded-md`（zcode-shell-specs §3 Select 规格）。设置页与模型选择器共用。
- `popover.go`：`func Popover(c *ui.Context, anchor *ui.Element, open *bool, fn func()) *ui.Element`——`ui.Popover`+`PopoverBase` 包装（`base.go:366-387`；空间不足自动翻到上方 `base.go:362-365`）；模型选择器弹层用。
- `tooltip.go`：**两条路径，不许混用**。①原生字符串版：`target.Tooltip(s string)`（`widgets.go:549-553`，只收字符串、样式随主题，禁用态提示与 SendButton 用它，避免 A→B 依赖）；②自绘版：`func Tip(c *ui.Context, target *ui.Element, content func())`（`ui.Overlay`/`PopoverBase` 实现，`base.go:366-387`：底 `bg-tooltip` 圆角 lg、字 `text-ui-sm`、可含键位 pill `bg-tooltip-tag` 与富内容）——工具卡错误详情（`max-w-96`、line-clamp 近似截断）与键位标签用 ②。
- `dialog.go`：`func Dialog(c *ui.Context, open *bool, opts DialogOptions) DialogResult`——`ui.Modal` 底座：遮罩 `black/60`、壳 `rounded-2xl border bg-popover p-4 shadow-md`、标题 `text-ui-base font-medium`、Footer 取消左/确认右（`dialog.tsx:58-140`）；`Confirm(c, open, ConfirmOptions) int` 紧凑确认变体（危险操作确认钮 destructive；会话删除、退出未发送确认用）；`Prompt(c, open, PromptOptions) (text string, result int)` 输入对话框变体（内嵌 lg 输入框，Enter 确认且忽略 IME 组合态，对齐旧栈 `TaskRenameDialog.tsx:27-95` 语义；会话重命名用）。Esc/Enter 键位与 `ui.AlertDialog`（`feedback.go:160-210`）行为对齐。
- `toast.go`：`func ShowToast(c *ui.Context, kind ToastKind, msg string)`——top-center 栈、3s 自动消失、`rounded-2xl border bg-toast`（**不透明近似**：ZCode `bg-toast/60+backdrop-blur-xl` 无原生等价，记录差异）；warning 变体带 `IconAlert`。

**C — 内容渲染（已改为 MujicaUI 官方组件接入）**

原计划自研的 C 流（goldmark 解析、chroma 高亮、自绘 toolcard/reasoning/bubble）已整体放弃，改用 mygo 作者的 **MujicaUI**（`github.com/ZacharyZhang-NY/MujicaUI`，MIT，组件文档 https://mujicaui.zacharyzhang.com/#components ）官方 chat/agent 分组组件，kit 只保留薄接入层。映射关系：

- 时间线：`chatlist.go` 的 `ChatList` → `chat.MessageList`（虚拟化、FollowEnd 跟尾、内置未读数「回到最新」按钮与「今天/昨天/日期」分隔，`Entry.Time` 喂 `Date` 选项）；运行中回合的合成行用 `chat.TypingIndicator`（"助手 正在输入…"）。
- 用户/助手消息：`chat.MessageBubble`（MessageUser 右对齐 Selection 底 / MessageAssistant 左对齐 Surface 底+细边），助手正文为 `chat.MarkdownView`（内部增量解析，流式直接喂增长后的全文；围栏未闭合按已收内容显示，取代原 markdown/ 子包与流式补齐逻辑；`Link()` 供后续接链接打开）。原用户气泡长文折叠（120px + 展开钮）随官方组件一并移除。
- 思考块：`reasoning.go` 的 `ReasoningBlock` → `chat.ThinkingBlock`（运行中 spinner + 实时秒数、完成「思考了 N 秒」、Collapsible 折叠、展开体自带左导线）。`ReasoningState{Running, Started, Elapsed, Text, Open}` 保留 caller-held 语义。
- 工具调用：`agent.ToolCallCard`，投影直接持有 `agent.ToolCall{ID, Name, Args, Result, Error, State, Duration}`——`tool_args` 的原始输入 JSON 进 `Args`（合法 JSON 自动格式化高亮），`tool_end` 的受限预览进 `Result`（失败进 `Error`，错误色）；`tool_start` 锚定耗时；denied/stopped → `AgentSkipped`。参数/结果各自独立折叠，展开状态存卡片元素上（依赖 MessageList 稳定 Key）。
- 日期分隔、行内距、节奏全部为官方 MessageList 自带（u*2/u*4 padding + u*2 gap），不再复刻 ZCode 的 56/20/gap-4/列宽常量。
- 错误横幅 `ErrorBanner` 保留自研（Mujica `chat.ErrorMessage` 的重试按钮在 v1 无运行时对应——重发会重复用户消息，运行时无 regenerate API）；「查看详情」弹 Dialog 流程不变。
- 草稿问候分支仍是产品自有布局（问候 + composer 居中），不采用 `chat.WelcomeScreen`（其为整页问候+能力卡片，无 composer 位）。
- 主题共存：`App.View` 每帧先 `core.Use(c, core.Settings{Mode, Locale: core.ZhCN})` 再 `theme.Apply`——MujicaUI 组件从 Use 安装的窗口本地令牌取色（酒红/象牙白官方外观），kit 自有控件仍从 c.Theme 读 zai 调色板，两套互不覆盖。

### 4.3 kit 测试（无窗口，`ui.NewTester`）

每实现者交付同目录 `*_test.go`：A——按钮点击改状态；**Editor 必测四序列**（本会话已在 /tmp 脚手架以 `go test -count=1` 全部跑通）：①`tt.Type("hello")`+`tt.Key(0, ui.KeyEnter)` → Submitted、草稿清空；②`tt.Key(ui.Shift, ui.KeyEnter)` → 不发送、草稿 `"hello\n"`；③`tt.Compose("ni",2)`+`tt.Type("你")`+`tt.Key(0, ui.KeyEnter)`（提交组合后再回车）→ 发送；④朴素「拦 Enter」谓词的**反向回归用例**（保留旧谓词断言 sent==1 复现，防退化）；B——Dialog/Confirm/Prompt 返回值、Select 选择、Toast 文案可见；C（MujicaUI 接入后）——时间线整链冒烟（MessageList 渲染/日期分隔/回到最新/TypingIndicator 行）、ThinkingBlock 折叠展开、ToolCallCard 参数/结果折叠、错误横幅回调；渲染 MujicaUI 组件的用例必须帧首 `core.Use`，折叠类断言先 `tt.SetPreferences(ui.Preferences{ReduceMotion: true})` 让动画单帧到位。范例：`examples/counter-native/main_test.go:11-34`、`ui/menu_test.go:14-96`。注意：输入框内容 Find 不到，可点控件加 `.Label()`（notes §9.5）；`NewTester(view, w, h)` 需显式尺寸，无焦点元素时按键不投递（本会话实测）。

---

## 5. 关键接口 Go 签名（并行实现者按此动手）

### 5.1 `internal/model`（消息与流事件，对照旧栈 `core/model/src/{message,stream,request}.rs`）

```go
package model

type Role string
const (
    RoleSystem    Role = "system"
    RoleDeveloper Role = "developer"
    RoleUser      Role = "user"
    RoleAssistant Role = "assistant"
)

// ContentBlock 密封接口；journal 持久化时按具体类型打 "type" 标签。
type ContentBlock interface{ isBlock() }
type TextBlock     struct{ Text string }
type ReasoningBlock struct{ Text string; Signature string } // Signature 原样回传（不透明续传状态）
type ToolCallBlock struct{ Call ToolCall }
type ToolResultBlock struct{ Result ToolResult }

type ToolCall struct {
    ID        string          // 响应内唯一
    Name      string
    Arguments string          // JSON 文本（原样拼接，不中途解析）
}
type ToolResult struct {
    CallID  string
    Content string
    IsError bool
}

type Message struct {
    IsMeta  bool             // 内部上下文：参与请求与持久化，不作用户发言展示（message.rs:230-236）
    Role    Role
    Content []ContentBlock
}

// 历史形状固定规则：一条 assistant 消息 = [ReasoningBlock(含签名), Text..., ToolCallBlock...]；
// 工具结果**紧随其后**回填为一条 Message{Role: RoleUser, Content: 仅 ToolResultBlock（按调用顺序）}。
// 旧栈 MessageRole::Tool（message.rs:18-19）在 Go 侧合并为 ToolResultBlock 并入 RoleUser 消息；
// 适配器拆装规则见 §5.2。

type ToolDefinition struct {
    Name        string         // 1-64 字节 ASCII 字母数字下划线短横线（tool.rs:49-52）
    Description string
    InputSchema json.RawMessage
}

type ToolChoice struct {
    Mode string // "auto" | "none" | "required" | "tool"
    Name string // Mode=="tool" 时有效
}

type ReasoningConfig struct{ Effort string } // "minimal"|"low"|"medium"|"high"

type ModelRequest struct {
    Model       string
    Messages    []Message
    Tools       []ToolDefinition
    ToolChoice  ToolChoice
    MaxTokens   int
    Temperature *float64
    Reasoning   *ReasoningConfig
}

type StopReason string
const (
    StopEndTurn       StopReason = "end_turn"
    StopToolUse       StopReason = "tool_use"
    StopMaxTokens     StopReason = "max_tokens"
    StopContentFilter StopReason = "content_filter"
    StopCancelled     StopReason = "cancelled"
)
// 归一策略（两适配器必须遵守）：端点原生原因 → 上述枚举一一对应（对齐旧栈
// usage.rs:87-103 Completed/ToolUse/MaxOutputTokens/ContentFilter/Cancelled/Other）；
// **未识别原因（Other，含 refusal 类）一律不发 MessageEnd，改发 EventError 携带
// 脱敏后的原始原因文本**，不静默映射为 end_turn。

type TokenUsage struct {
    InputTokens, OutputTokens, CachedTokens int64 // 未知为 -1
}

type StreamEventType string
const (
    EventMessageStart     StreamEventType = "message_start"
    EventTextDelta        StreamEventType = "text_delta"
    EventReasoningDelta   StreamEventType = "reasoning_delta"
    EventReasoningSummaryDelta StreamEventType = "reasoning_summary_delta"
    // ReasoningContinuation 携带一段推理用于后续请求连续性的**不透明签名状态**
    //（对齐旧栈 stream.rs:39-53 的 ReasoningSummaryDelta / ReasoningContinuation）；
    // Anthropic thinking+工具多轮必需回传，OpenAI 无签名则适配器不发此事件。
    EventReasoningContinuation StreamEventType = "reasoning_continuation"
    EventToolCallStart    StreamEventType = "tool_call_start"
    EventToolCallArgsDelta StreamEventType = "tool_call_args_delta"
    EventToolCallEnd      StreamEventType = "tool_call_end"
    EventUsage            StreamEventType = "usage"
    EventMessageEnd       StreamEventType = "message_end"
    EventError            StreamEventType = "error"
)

// StreamEvent 扁平结构；Index 为响应内稳定内容块序号（stream.rs:17-100 对齐）。
type StreamEvent struct {
    Type        StreamEventType
    Index       uint32
    Delta       string     // Text/Reasoning/Summary/Args 增量
    Continuation string    // ReasoningContinuation：原样持久化与回传的不透明签名
    CallID      string
    Name        string     // ToolCallStart
    Usage       TokenUsage
    StopReason  StopReason // MessageEnd
    Err         error      // EventError（流中断/HTTP/解析），之后通道关闭
}

type ModelResponse struct {
    Content    []ContentBlock
    StopReason StopReason
    Usage      TokenUsage
    Model      string
}
```

### 5.2 `internal/provider`（Provider 接口，对照 `core/model/src/provider.rs:79-97`）

```go
package provider

// Provider 是协议中立的模型网关。实现不得修改 req；
// ctx 取消必须终止 HTTP 连接、发出 EventError 后关闭事件通道。
type Provider interface {
    // Capabilities 返回模型能力快照（推理支持、结构化输出、上下文窗口）。
    Capabilities(model string) Capabilities
    // Stream 校验请求并发起一次流式调用。校验失败立即返回 error；
    // 成功时返回事件通道，通道在 EventMessageEnd 或 EventError 后关闭。
    Stream(ctx context.Context, req model.ModelRequest) (<-chan model.StreamEvent, error)
}

type Capabilities struct {
    Reasoning        bool
    ReasoningEfforts []string
    ContextWindow    int64
    MaxOutputTokens  int64
}

// Complete 收集一次流式调用为完整响应（等价旧栈 collect_model_stream：
// 校验事件顺序，流中断时返回携带已收文本的部分响应错误）。
func Complete(ctx context.Context, p Provider, req model.ModelRequest) (model.ModelResponse, error)

// Protocol 协议标识；第一版仅 Anthropic 与 OpenAIChat。
type Protocol string
const (
    ProtocolAnthropic Protocol = "anthropic"       // /v1/messages，SSE
    ProtocolOpenAI    Protocol = "openai-chat"     // /chat/completions，SSE
)

// Endpoint 运行时端点（由 config.ProviderConfig 映射，# 语义已解析）。
type Endpoint struct {
    Protocol Protocol
    BaseURL  string // 已含 /v1 或完整路径
    APIKey   string
}

// New 构造对应协议适配器；未知协议返回错误。
func New(ep Endpoint) (Provider, error)
```

适配器职责（对齐 `AGENTS.md` 协议职责条款）：
- 请求形状、SSE 解析、增量合并、工具调用拼装、Usage 归一、错误归一（429/5xx/超时→`EventError` 可重试标记）。
- **历史拆装（两条适配器都必须实现并测试）**：Go 历史中「紧随 assistant 的 RoleUser 消息仅含 ToolResultBlock」（§5.1 规则）→ Anthropic 侧转为该 user 消息内的 `tool_result` 块（协议要求 tool_result 跟在对应 assistant 的 tool_use 之后）；OpenAI 侧拆成逐条 `role=tool` 消息（`tool_call_id` 对应）。
- **推理签名**：Anthropic thinking 块的 `signature_delta` → `EventReasoningContinuation{Continuation}`；历史中带签名的 ReasoningBlock 原样回传下一轮请求。OpenAI 无签名机制，不发该事件。
- 未识别的结束原因不发 MessageEnd，改发 EventError（§5.1 归一策略）。
- 每适配器带 `wire_test.go`（固定 JSON 夹具→事件序列）。

### 5.3 `internal/tools`（Tool 接口与注册表，对照 `core/agent/src/tool.rs:1062-1160`）

```go
package tools

type Effect string
const (
    EffectReadOnly   Effect = "read_only"   // 自动放行
    EffectSideEffect Effect = "side_effect" // 需用户确认；fail-closed：判定不了按副作用
)

type Invocation struct {
    CallID  string
    Name    string
    Input   json.RawMessage     // Registry 已按 Definition.InputSchema 校验
    WorkDir string              // 项目根：相对路径解析与越界校验边界
    Emit    func(progress string) // 长执行进度（Bash 输出预览），可为 nil
}

type ToolOutput struct {
    Content string // 给模型的文本结果（截断到预算内）
    IsError bool
    Summary string // 工具卡摘要（primaryText/secondaryText 契约）
}

type Tool interface {
    Definition() model.ToolDefinition
    // Effect 按本次输入声明类别；实现必须 fail-closed。
    Effect(input json.RawMessage) Effect
    Execute(ctx context.Context, inv Invocation) (ToolOutput, error)
}

type Registry struct{ /* name → Tool，冻结排序 */ }
func NewRegistry(tools ...Tool) (*Registry, error) // 重名/定义非法返回错误
func (r *Registry) Definitions() []model.ToolDefinition
func (r *Registry) Get(name string) (Tool, bool)
```

第一版工具六件（名称与旧栈一致：`filesystem.rs:58,129,195`、`command.rs:68`、`search.rs:48,118`）：`NewRead(workDir)`、`NewWrite(workDir)`、`NewEdit(workDir)`、`NewGlob(workDir)`、`NewGrep(workDir)`、`NewBash(workDir, timeout)`。Read/Glob/Grep 恒 `EffectReadOnly`；Write/Edit 恒副作用；Bash 恒副作用（v1 不做输入级只读判定，保守 fail-closed）。

### 5.4 `internal/agent`（Agent Loop，对照 `core/agent/src/runner.rs:280-347` TurnRequest/轮次机）

```go
package agent

// Dependencies Loop 的全部外部能力；UI 无关。
type Dependencies struct {
    Provider provider.Provider
    Tools    *tools.Registry
    System   model.Message            // 系统提示词（app 由 prompt.go 组装）
    // Authorize 副作用工具执行前回调；false=拒绝（产生 denied 状态事件）。
    // 实现可阻塞（原生对话框），Loop 在独立 goroutine 调用。
    Authorize func(ctx context.Context, req PermissionRequest) (bool, error)
    MaxRounds int                      // 兜底轮数，v1 = 32，防失控
}

type PermissionRequest struct {
    SessionID, TurnID, CallID, ToolName string
    Input   json.RawMessage
    Summary string                    // 对话框正文摘要
}

type Agent struct{ deps Dependencies }
func New(deps Dependencies) *Agent

// RunTurn 执行一个完整 Turn：模型流 → 事件透传（含 EventReasoningContinuation，
// 其 Continuation 在落 assistant 历史时并入对应 ReasoningBlock.Signature，下一轮
// 原样回传——Anthropic thinking+工具多轮的硬要求）→ 收集工具调用 → 逐个授权执行
// → 结果按 §5.1 规则回填为紧随 assistant 的 RoleUser(仅 ToolResultBlock) 消息 →
// 下一轮；StopReason==end_turn 或取消时返回。
// 每个 Event 先经 append（journal，可 nil）再 emit；emit 返回 error 时中止。
// ctx 取消：停止模型流/杀进程组，补 TurnFailed(取消) 事件，返回 nil。
func (a *Agent) RunTurn(ctx context.Context, req TurnRequest, append func(model.Event) error,
    emit func(model.Event) error) error

type TurnRequest struct {
    SessionID string
    TurnID    string
    Model     string
    History   []model.Message   // 含系统提示词与已持久化全部消息
    WorkDir   string
}
```

事件统一为 `model.Event`：`EventType` 类型与全部种类常量**同在 model 包定义**（避免 model→agent 反向依赖；`agent/event.go` 仅做别名与 agent 内部辅助）：

```go
type Event struct {
    ID        string    // 幂等身份（重投不变）
    SessionID string
    TurnID    string
    Seq       int64     // journal 序号；回放与直播全流单调连续，UI 以此幂等去重兜底
    Replay    bool      // true=订阅回放的历史事件；live 事件恒为 false
    Time      time.Time
    Type      EventType // "user_message" "text_delta" "reasoning_delta" "reasoning_continuation"
                        // "tool_args" "tool_end" "tool_result" "usage" "turn_completed"
                        // "turn_failed" "turn_cancelled" "permission_denied" "tool_start"
                        // 注：reasoning_continuation 的 Text = 不透明签名，**必须落 journal**
                        // 并在重放重建历史时并入当前 ReasoningBlock.Signature（否则重启后
                        // Anthropic thinking+工具多轮回传缺签名即失败）；provider 流的
                        // summary_delta 不转发到 journal（UI 折叠摘要取最后一行非空文本）
    Text      string        // delta 文本 / 错误消息 / 标题
    Tool      *ToolEvent    // 工具卡片投影：CallID/Name/Summary/Status/Output 预览
    Usage     *model.TokenUsage
    StopReason model.StopReason
}
type ToolEvent struct {
    CallID, Name, Summary string
    Status    string // pending|running|completed|failed|denied|stopped
    Detail    string // 展开详情（命令/路径/输出预览）
    AddedLines, RemovedLines int // Edit/Write 变更统计
}
```

`prompt.go`：`func BuildSystemPrompt(workDir, platform string) model.Message`——v1 固定中文模板（身份、工作目录、工具使用规则、当前日期），无 Plan/协作/记忆段落。

### 5.5 `internal/runtime`（Session / journal / 事件订阅）

```go
package runtime

type SessionMeta struct {
    ID         string    // 时间有序 128 位随机 ID（crypto/rand + Crockford Base32，标准库自实现、ULID 兼容格式，不引第三方）
    Title      string    // 首条用户消息截断 40 字符（runes）
    ProjectDir string
    CreatedAt  time.Time
    UpdatedAt  time.Time
}

type Manager struct{ /* root, mu, map[id]*Session */ }
func OpenManager(root string) (*Manager, error)                  // root = ~/.keencode/go-v1/sessions
func (m *Manager) Create(projectDir string) (*Session, error)    // 建目录+meta.json
func (m *Manager) List() ([]SessionMeta, error)                  // UpdatedAt 倒序
func (m *Manager) Rename(id, title string) error
func (m *Manager) Delete(id string) error                        // 运行中拒绝
func (m *Manager) Get(id string) (*Session, error)               // 惰性重放 journal 恢复

type Session struct{ /* meta, journal, hub, cancel, agent */ }
func (s *Session) Meta() SessionMeta
func (s *Session) History() []model.Event          // 重放出的事件（含用户消息）
func (s *Session) Running() bool
// Send 追加 user_message 事件并启动一个 Turn goroutine（运行中返回 ErrBusy）。
func (s *Session) Send(ctx context.Context, text string) error
func (s *Session) Stop()                            // 取消当前 Turn（幂等）
// Subscribe 返回实时事件通道与退订函数。
// 无缝契约（实现必须满足）：hub 分发与 journal append 共用同一把互斥锁；
// Subscribe 在**持锁状态下**先注册订阅者，再逐条投递 History()（事件带
// Replay=true），解锁后新事件才进入直播——回放与直播不丢不重、无竞态窗口。
// 每个订阅者带独立无界队列 goroutine，慢消费不阻塞分发。
// UI 端仍应以 Event.Seq 做幂等去重兜底（Replay 与 live 全流 Seq 单调连续）。
func (s *Session) Subscribe() (<-chan model.Event, func())
```

- `journal.go`：`journal.jsonl` 每行一个 `model.Event`（标准库 `encoding/json` v1 API；**不用 `encoding/json/v2`**——本机 go1.26.5 实测不可导入，见风险 7；`Seq` 单调递增）；追加即 O_APPEND write；**fsync 策略**：用户消息与 Turn 终态事件立即 fsync，delta 类事件攒 100ms 批量。恢复 = 顺序重放；**半行容错**：末行 JSON 不完整则截断（先读出合法前缀行数再 truncate）。重启后重放遇未闭合 Turn：内存补 `turn_failed("应用中断")` 投影，不改写 journal。
- `draft.go`（**草稿持久化**，支撑 §6 验收第 5 条「草稿跨重启保留、会话切换不丢」）：

  ```go
  // 会话草稿：sessions/<id>/draft.txt（0600）；输入防抖 500ms 落盘，空文本=删除文件。
  func (m *Manager) SessionDraft(id string) (string, bool)
  func (m *Manager) SaveSessionDraft(id, text string) error
  // 新会话草稿（草稿态尚未有 session）：~/.keencode/go-v1/draft-new.json（0600），
  // 结构 {"projectDir":"","text":""}；同样防抖落盘，草稿清空且无目录=删除文件。
  func (m *Manager) NewDraft() (projectDir, text string, ok bool)
  func (m *Manager) SaveNewDraft(projectDir, text string) error
  ```

  **草稿 → 会话转换时机**：新建任务只进入 UI 草稿态（持 `{projectDir, draftText}`，**不建目录不落 meta**，对齐 zcode-shell-specs §2.1「点击进入草稿态而非立即建任务」）；`Manager.Create(projectDir)` 延迟到**该草稿的首次 Send**，随即写 meta（标题=首条消息）并追加 user_message 事件。
  **防抖竞态防护**：`Send` 的 projectDir 由调用方传**内存态**（见 §5.7），不回读防抖文件；`draft.go` 另提供 `func (m *Manager) FlushDrafts()`——窗口关闭、会话切换、编辑器失焦前 app 层必须先调用，把未落盘的防抖缓冲写穿。
- 持久化布局（D4）：
  ```
  ~/.keencode/go-v1/config.json                    # 0600：主题、供应商、默认模型
  ~/.keencode/go-v1/draft-new.json                 # 0600：新会话草稿
  ~/.keencode/go-v1/sessions/<id>/meta.json        # 0600：标题/项目目录/时间戳
  ~/.keencode/go-v1/sessions/<id>/journal.jsonl    # 0600：事件日志
  ~/.keencode/go-v1/sessions/<id>/draft.txt        # 0600：该会话未发送草稿
  ```
- 标题规则：首条 `user_message` 事件 → 去空白 → 前 40 runes → 写 `meta.json`（之后用户重命名覆盖）。

### 5.6 `internal/config`

```go
package config

type ThemeSetting string
const (ThemeSystem ThemeSetting = "system"; ThemeLight = "light"; ThemeDark = "dark")

type ModelConfig struct{ ID, DisplayName string }
type ModelRef struct{ ProviderID, ModelID string }

type ProviderConfig struct {
    ID       string        // 用户可读标识（1-64，字母数字._-，providers.rs:1285-1298 同规则）
    Name     string
    Protocol string        // "anthropic" | "openai-chat"
    BaseURL  string        // 见 ResolveURL # 语义
    APIKey   string
    Models   []ModelConfig
}

// ResolveURL 校验并解析 base URL（完整移植旧栈 validate_base_url +
// validate_exact_endpoint，providers.rs:1298-1349），规则全部五条：
//   - 末尾单独 "#" = 完整路径标记：地址即最终请求端点，运行时不再追加
//     "/v1" 或协议端点后缀；标记原样保留在持久化值中，仅在映射 Endpoint 时剥离；
//   - **`#` 结尾地址必须以所选协议的端点后缀结尾**（anthropic=/messages、
//     openai-chat=/chat/completions），否则**配置期即报错**（旧栈
//     validate_exact_endpoint，providers.rs:1333-1349）——防止 `https://host/v1/chat#`
//     这类错配地址拖到请求期 404；
//   - 命名片段（#section）报错；"#" 结尾但路径为空报错；
//   - 无 "#" 且仅域名 → 自动补 "/v1"；显式路径保持不变；
//   - http/https 且 host 非空，否则报错。
func (p ProviderConfig) ResolveURL() (provider.Endpoint, error)

// ModelCatalogURL 模型目录地址（providers.rs:1352-1372 同规则：对 "#" 完整路径
// 剥离标记与协议端点后缀，在与生成端点同级的目录查询）。第一版模型列表由用户
// 手动维护、不拉取目录；本函数仅随 Save 校验保留规则，W2 之前不实现网络调用。
func (p ProviderConfig) ModelCatalogURL() (string, error)

type Config struct {
    Theme        ThemeSetting      // 默认 "dark"（zai-dark 为产品默认）
    Providers    []ProviderConfig
    DefaultModel ModelRef
}
func DefaultConfig() Config

type Store struct{ /* path, mu */ }
func OpenStore(path string) (*Store, error)   // 不存在则落 DefaultConfig
func (s *Store) Load() (Config, error)
func (s *Store) Save(cfg Config) error        // 临时文件 + rename 原子写，0600

func DefaultRoot() (string, error)            // $HOME/.keencode/go-v1（KEENCODE_GO_HOME 可覆盖，测试隔离）
```

### 5.7 `internal/app` 服务层（main 装配；方法集保持 mygo.Bind 兼容形状，见 D2）

```go
package app

// Services 服务集合：main 构造一次，注入 UI 状态根。
type Services struct {
    Settings *SettingsService
    Sessions *SessionService
    Project  *ProjectService
}

// SessionService —— CRUD/投递方法集 Bind 兼容（导出、JSON 可编码、error 结尾）。
type SessionService struct{ mgr *runtime.Manager; deps func() agent.Dependencies }
func (s *SessionService) Create(projectDir string) (runtime.SessionMeta, error)
func (s *SessionService) List() ([]runtime.SessionMeta, error)
func (s *SessionService) Rename(id, title string) error
func (s *SessionService) Delete(id string) error
// Send id=="" 表示从新会话草稿态发起：以调用方传入的 projectDir（UI 内存态，
// 不回读 500ms 防抖文件，避免选完目录立即回车取到旧目录）Manager.Create
// 落盘建会话（草稿→会话转换，见 §5.5）后发送；id!="" 时 projectDir 忽略。
// 窗口关闭/会话切换/失焦前，app 层先 mgr.FlushDrafts()。
func (s *SessionService) Send(id, projectDir, text string) error
func (s *SessionService) Stop(id string) error
// 草稿（§6 验收第 5 条「跨重启保留、会话切换不丢」的服务面）：
func (s *SessionService) SaveDraft(id, text string) error // id=="" 存新会话草稿
func (s *SessionService) Draft(id string) (string, error) // id=="" 读新会话草稿
type NewDraft struct{ ProjectDir, Text string }           // JSON 可编码
// Events 事件订阅：进程内 channel 形态，不承诺 Bind 兼容
//（Bind 的流式参数需 *mygo.Channel[T]，接 web 表面时再包一层）。
// 契约：通道先收到 Replay=true 的历史回放、后收到直播事件，无丢重（§5.5）。
func (s *SessionService) Events(id string) (<-chan model.Event, func(), error)

type SettingsService struct{ store *config.Store }
func (s *SettingsService) Get() (config.Config, error)
func (s *SettingsService) UpsertProvider(p config.ProviderConfig) error
func (s *SettingsService) DeleteProvider(id string) error
func (s *SettingsService) SetDefaultModel(ref config.ModelRef) error
func (s *SettingsService) SetTheme(t config.ThemeSetting) error

type ProjectService struct{ win func() *mygo.Window }
// ChooseDirectory 弹原生目录选择（Dialog.Open{Directory:true}，dialog.go:24-44）。
// 必须在 goroutine 调用（阻塞 API）；Parent 绑主窗口成 macOS sheet。
func (p *ProjectService) ChooseDirectory() (string, error)
```

`cmd/keencode/main.go` 装配序：

```go
func main() {
    root, _ := config.DefaultRoot()
    store, _ := config.OpenStore(filepath.Join(root, "config.json"))
    mgr, _ := runtime.OpenManager(filepath.Join(root, "sessions"))
    cfg, _ := store.Load()
    svcs := app.Assemble(store, mgr) // 服务互连 + Authorize 桥（permission.go）
    mygo.App.WhenReady(func() {
        applyThemeSource(cfg.Theme)  // mygo.Theme.SetSource(ThemeDark|Light|System)
        win := mygo.NewWindow(mygo.WindowOptions{
            Title: "KeenCode", Width: 1280, Height: 800,
            MinWidth: 720, MinHeight: 480,
            StateKey: "main",                 // 记住窗口位置/大小（window.go:152-159）
            Content:  ui.View(app.Root(svcs).View),
        })
        svcs.BindWindow(win)                  // ProjectService.Parent 用
    })
    if err := mygo.App.Run(); err != nil { log.Fatal(err) }
}
```

`permission.go` 的 Authorize 桥：Turn goroutine → `mygo.Dialog.Message(MessageOptions{Parent: win, Type: Question, Buttons: ["允许","拒绝"], DefaultButton: 0 /* Enter=允许，对齐 ZCode Enter=确认习惯 */, CancelButton: 1 /* Esc=拒绝 */, Message: 摘要, Detail: 命令/路径})` → bool。mygo 跨线程调用自动转主线程等待（`mygo.go:44-49`），无需额外同步。

---

## 6. 第一版产品范围（每日可用验收清单）

| # | 能力 | 验收场景 | 落点 |
| --- | --- | --- | --- |
| 1 | 单窗口 | 1280×800 启动；拖动/缩放后重启位置尺寸记忆（StateKey） | `main.go`、`window.go:152-159` |
| 2 | 亮暗主题 | 跟随系统切换即时换肤；设置固定 light/dark/system 并持久化；zai 两面令牌值与 `zcode-tokens.md` §2 一致 | `theme/palette.go`、`bridge.go`、settings |
| 3 | 会话管理 | 新建（进入草稿态，**不建目录不落 meta**，对齐 zcode-shell-specs §2.1；首次 Send 才 Create，见 §5.5 转换时机）；右键重命名（Prompt 对话框，B 流）；删除（Confirm destructive）；重启后列表与全部消息恢复（journal 重放），列表按 UpdatedAt 倒序 | `runtime/manager.go`、`app/shell.go` |
| 4 | 聊天流式渲染 | 用户气泡右对齐（rounded-xl rounded-tr-xs bg-surface）；助手 markdown（标题/列表/引用/行内代码/代码块+高亮+复制）；思考块默认收起、运行中「思考中」；工具卡片摘要行+展开+状态词；错误横幅（输入区上方，查看详情 Dialog）；停止（发送位变停止钮；**Esc 经 `c.Shortcut(0, ui.KeyEscape)` 实现，归属 `app/chat.go`，W3**——无 modal 时生效、对话框打开时被遮挡自动失效，`context.go:276-284` + `scope.go:152-160`） | `kit/{markdown,inline,codeblock,reasoning,toolcard,message,chatlist}.go`、`app/chat.go` |
| 5 | 多行输入 | Enter 发送、Shift+Enter 换行、**IME 组合回车只上屏不发送**（实现走 §4.2 帧时缓冲差分；headless 四序列 + **W4 真机 IME 验收**双重把关）；草稿跨重启保留、会话切换不丢（`runtime/draft.go` 防抖落盘 + FlushDrafts + `app/services.go` SaveDraft/Draft，见 §5.5）；占位符**两分支**：无历史「描述新任务」/有历史空闲「继续追问」（运行中不改占位语——「排队追问」分支随队列发送一起推迟，见 §7） | `kit/editor.go`、`runtime/draft.go`、`app/{chat,services}.go` |
| 6 | 项目目录 | 新建会话前选目录（原生目录选择器）；显示于顶栏；作为 WorkDir 传工具 | `ProjectService`、`app/shell.go` |
| 7 | 供应商与模型设置 | 自定义供应商增删改（名称/协议 anthropic·openai-chat/base URL 含 `#` 完整路径语义/API Key/模型列表）；默认模型选择；composer 模型选择器（Popover 列出全部 ProviderConfig.Models）；设置即存即生效 | `config`、`app/settings.go`、`kit/select.go` |
| 8 | 工具权限 | Read/Glob/Grep 自动执行无弹窗；Write/Edit/Bash 每次弹原生**两态**对话框（允许/拒绝；Enter=允许、Esc=拒绝——对齐 ZCode ConfirmDialog「Enter=确认」习惯，`ConfirmDialog.tsx:150-190`；如需 fail-closed 默认可对调 DefaultButton，见 §10 差异 9）；拒绝产生 denied 工具卡，模型可继续 | `tools/*.Effect`、`app/permission.go` |
| 9 | 标题 | 首条用户消息前 40 字符自动命名；手动重命名覆盖 | `runtime/session.go` |
| 10 | 稳定性 | 强杀进程重启：半行 journal 截断恢复、未完成 Turn 投影为「应用中断」、会话列表完整 | `journal.go` |

**预算（对齐 AGENTS.md）**：安装二进制 ≤ 50MB（mygo 骨架实测 15MB，notes §1.3）；空闲 CPU ≈ 0（空闲不渲染）；冷启动首帧 < 1.5s（原生无 webview，预期富余）。

---

## 7. 推迟清单 deferredScope（第一版明确不做）

- **Plan 模式 UI**：模式切换与只读守卫界面；Loop 先留 PlanGuard 挂点但不实现。
- **子代理**：Spawn/Followup 等协作工具与子代理卡片；单层限制语义推迟到 V2。
- **记忆/目标**：`~/.keencode/memories` 读写与 Goal/Todo 工具、投影。
- **Skills**：技能目录扫描、`skill` 工具与市场入口。
- **MCP**：MCP 客户端、工具桥接与设置页。
- **web-host / Web 端**：远程访问服务器与浏览器界面（服务层 Bind 兼容已为其留形，见 D2）。
- **多窗口**：第二窗口/分屏；单窗口单会话。
- **检查点/回滚**：文件快照、fork、消息编辑重发（用户气泡的编辑钮不实现）。
- **会话分享**：导出与只读分享页。
- **更新器**：自动更新检查与安装（`mygo updater` 模块存在但第一版不接）。
- **i18n 抽取**：界面文案内联中文；键值抽取与 zh-TW/en 推迟（届时 en 为键权威）。
- **附件/图片**：文件与图片上传、剪贴板贴图、拖放（`DroppedFiles` API 已存在）。
- **OpenAI Responses 协议**：第三适配器（`internal/provider` 目录结构已预留）。
- **Mermaid**：图渲染与预览钮。
- **队列发送**：运行中追加消息排队与队列确认 Dialog（运行中 Send 直接返回 ErrBusy）。
- **思考深度切换**：ReasoningEffort 循环控件（默认 medium）。
- **上下文用量显示**：composer 用量面板。
- **工时折叠条/系统分隔线**：AssistantHistoryStatus、compact/modelChange 分隔。
- **草稿推荐提示与问候语动画**：静态问候文案保留，瀑布入场/逐项延迟不实现。
- **桌面通知/托盘/全局快捷键**：`mygo.NewNotification`、托盘、`c.Shortcut` 全局热键注册。
- **diff 视图**：文件变更摘要以数字行显示（+N/−N），完整 diff 渲染器推迟。
- **终端面板**：主区下方终端（mygo 有 `plugins/terminal` 可嵌，V2 评估）。
- **API Key keyring**：系统钥匙串存储（D6 明文 0600）。
- **会话搜索/归档/置顶/时间线分组**：侧栏仅 UpdatedAt 平铺列表。
- **上下文压缩**：长会话超窗直接 `turn_failed`，错误横幅给固定文案「对话过长，请新建会话后重试」（§6 第 4 条）；旧栈 ContextCompaction 事件族（`core/agent/src/event.rs:681-699`）与压缩分隔线推迟到 V1.1+。
- **模型目录拉取**：模型列表由用户在设置中手动维护；`ModelCatalogURL`（§5.6）仅保留 `#` 剥离规则备查，第一版不发网络请求。
- **流式逐字动画**：新内容 900ms 入场动画（`zcode-stream-text-in`）不实现，正文直接更新（对齐"无打字光标"决策）。

---

## 8. 波次路线图

### 本工作流 = 第一版（四个波次）

| 波次 | 内容 | 并行性 | 出口判据 |
| --- | --- | --- | --- |
| **W0 冻结层**（先行，串行小步） | `go.mod`（**必须声明 `go 1.27.1`**——mygo 硬要求；构建统一 `GOTOOLCHAIN=auto`；**mygo 锁定 commit `459b512`（"Release 0.2.10"）的伪版本**——上游无 v0.2.10 tag，本地 `git tag` 实测止于 v0.2.9，见风险 8）+ 第三方依赖清单（**mygo** 框架；**goldmark** MIT，Markdown AST；**chroma/v2** 主许可 MIT（COPYING 首段，Copyright (C) 2017 Alec Thomas，本会话经 GitHub API 核实），文件内捆绑内嵌字体等第三方数据许可（SIL OFL 等）——全量 lexer 预计带来 ~10MB 级二进制增量，W2 实测，超预算则降级为仅注册常用语言；会话 ID 用标准库自实现，不引 ulid）+ `internal/model` 全部类型、`internal/provider/provider.go`（Endpoint/Protocol——config.ResolveURL 返回该类型，不进 W0 则 config 编译不过）、`internal/ui/theme`（palette/scale/bridge）、`kit/kit.go` 契约（含 PromptOptions）、`internal/config` | 单人顺序提交，1-2 天 | `go vet ./...` 过；palette 值与 zcode-tokens.md §2 逐项对照通过；go.mod 无 replace、无 tag 依赖（pin 伪版本） |
| **W1 kit 三流**（并行） | A 基础控件 8 文件 / B 浮层 6 文件 / C 内容渲染 7 文件，各自带 headless 测试 | **3 人全并行**（依赖仅 W0 冻结层） | 每文件有测试；`go test ./internal/ui/kit/...` 绿；三流无交叉文件改动 |
| **W2 运行时内核**（并行） | P1：`internal/provider/anthropic`；P2：`internal/provider/openai`；P3：`internal/tools` 六件；P4：`internal/agent` Loop + `internal/runtime` journal/manager（P4 与 P1-P3 间以 §5 签名为界，用 fake Provider/Tool 测试） | 4 路并行 | wire 夹具测试、工具集成测试、Loop 用 Scripted Provider 全分支覆盖（对齐旧栈 `core/model/src/scripted.rs` 思路） |
| **W3 装配与界面**（收口） | `internal/app`（shell/chat/settings/permission）+ `cmd/keencode/main.go` + 服务互连 | 1-2 人 | 真实 API Key 手工冒烟：会话全流程 §6 十项逐条过 |
| **W4 验收** | 像素对照（mygo `tt.Image()` 快照 vs ZCode 规格）、性能记录（启动/内存/空闲）、有意差异清单定稿 | 1 人 | §6 验收全绿 + 差异清单归档本文档附录 |

### 后续工作流扩展

- **V1.1 体验补齐**：队列发送、思考深度、上下文用量、工时折叠条、diff 视图、会话搜索、i18n 抽取。
- **V2 能力面**：Plan 模式 UI、子代理、Skills、MCP、记忆/目标、检查点回滚、OpenAI Responses 适配。
- **V3 平台面**：web-host（复用 Bind 兼容服务层 + `mygo.Bind` 生成 TS 客户端）、多窗口、托盘/通知/更新器、附件图片。
- 每个工作流沿用本文档的令牌与 kit 契约；扩 kit 时按 §4.2 依赖规则归口实现者。

---

## 9. 风险与缓解

| # | 风险 | 证据 | 缓解 |
| --- | --- | --- | --- |
| 1 | **mygo 富文本边界**：`ui.Span` 只有排版字段、无交互 | `richtext.go:15-33` | 需要交互的行内元素（链接、可点代码）改用 `RichText(c).Children(...)` 拼 `ui.Text`/`ui.Link`（`context.go:131-133` 限定子元素为文本类，命中区域跨行跟随）；Span 与 Children 两套 API 已在 §4.2 inline.go 明确分工 |
| 2 | **视觉近似偏差**：渐变扫光、backdrop-blur、Radix 高度动画、遮罩模糊、气泡底部渐隐 mask 无原生等价 | `styles.css:838`（gradient-flow）、`dialog.tsx:28-40`、`reasoning.tsx:190-207` | 以 mygo `Animate/Loop`（`widgets.go:50-107`）做呼吸/位移近似；toast/遮罩提高不透明度；三类折叠直接切换（省 300ms 延迟卸载）；全部记入「有意差异清单」，不静默近似 |
| 3 | **语法高亮色差**：chroma github 风格 ≠ shiki github-light/dark 逐 token 等值 | `zcode-tokens.md` §10（shiki 主题 JSON 未提取） | v1 接受近似（同族色相/明度）；V1.1 提取 shiki 主题 JSON 生成 Go 映射表 |
| 4 | **IME 组合态回车误发送**：macOS `keyDown:` 先无条件上报 KeyPressed 再交输入法（`internal/darwin/surface.go:701-709`），组合态上屏的回车也会先以 `InputKeyDown+KeyEnter` 到达 fn（`input.go:53→487-495` 无组合态守卫）；且带内建编辑器的元素 `InputCompose/InputText` 不投 fn（`input.go:688-694`），「fn 内自维护组合态」不可行 | 本会话 /tmp 脚手架 headless 实测：朴素拦 Enter 谓词 sent=1（复现误发送）；组合态跟踪谓词同样 sent=1；帧时缓冲差分方案三用例全过 | §4.2 editor.go 的**帧时缓冲差分**设计（快照→放行→帧内判定 `draft==snapshot+"\n"`）；真机 IME 验收列入 W4；建议向上游提 composition 查询 API issue |
| 5 | **流式事件洪水**：TextDelta 高频到每秒数百，直接 `win.Update` 会掉帧 | notes §2.2 重绘触发 | app 泵 16ms 合帧（批量 drain channel 后一次 Update）；journal delta 攒 100ms 批量 fsync |
| 6 | **长会话性能**：数千事件重放与渲染 | `ui.List` 只构建可见行±2、未显示行高估算（`list.go:249-257`）；TextArea 按段虚拟化 | 消息流全程 `ui.List` 虚拟化；`ListState.Key=msgID` 保滚动锚定；恢复大会话首帧只重放最近 N 条 + 按需向前加载（`ScrollIntoView` 锚点平移） |
| 7 | **工具链版本**：mygo 要求 go 1.27.1，本机 1.26.5；且 `encoding/json/v2` 在 go1.26.5 默认不可用 | `mygo/go.mod:3`；本会话 `go version` = go1.26.5；本会话 `go doc encoding/json/v2` 实测 "cannot find package" | go.mod 锁 `go 1.27.1` + `GOTOOLCHAIN=auto` 自动拉取（notes §1.3 已实证）；journal 因此用标准库 `encoding/json`（v1 API，§5.5）；CI 显式安装 go1.27.x |
| 8 | **mygo 无 v0.2.10 tag**：`mygo.go:70` 的 Version="0.2.10" 只是常量；本地 `git tag` 实测止于 **v0.2.9**，"Release 0.2.10"（`459b512`）是未打 tag 的提交且其后还有新提交（本会话 `git log` 实证；评审方 `git ls-remote` 同证） | 按 tag `v0.2.10` 执行 `go get`/CI 会失败 | W0 锁定 commit 伪版本（`go get github.com/egoist/mygo@459b512` 生成的 pseudo-version）；本地开发用 go.work 指向检出；go.sum 拷贝 mygo 仓库全量（缺条目实测报 `missing go.sum entry`）；CI 可拉性依赖网络环境，W0 首日验证；上游打 tag 后切正式版本 |
| 9 | **三人并行冲突**：kit 三流共享图标与变体契约 | §4.1-4.2 | kit.go 契约在 W0 冻结并在本文档给全签名；文件归属到人、禁止跨界改文件；缺契约先提 A 补 |
| 10 | **journal 损坏**：半行/磁盘满 | §5.5 | 每会话独立目录 + 尾行截断恢复 + 追加前尺寸检查；config 原子写 |
| 11 | **像素级验收无浏览器 DevTools**：原生 UI 无法用 CSS 检查器 | — | 用 mygo `tt.Image()`/`ui.Render` 出帧与 ZCode 规格数值对照（间距/字号/颜色按 §3 常量断言），差异以「有意差异清单」显式管理（AGENTS.md "记录有意差异"） |
| 12 | **平台差异**：连续圆角仅 macOS、菜单弹出时机、Wayland 快捷键受限 | notes §9.8 | 第一版验收平台 macOS；差异记清单；快捷键统一走 `ui.Cmd` 抽象（`keys.go:112`） |

---

## 10. 有意差异清单（初稿，W4 定稿归档）

1. ~~扫光文字 → 呼吸透明度动画（风险 2）~~（2026-10-06 随聊天组件改用 MujicaUI 官方组件而失效，ShimmerText 已删除）。
2. Toast/对话框遮罩：不透明近似，无 backdrop-blur（风险 2）。
3. ~~折叠展开无 300ms 高度动画，直接切换（风险 2）~~（2026-10-06 失效：Mujica Collapsible 自带高度动画，减少动画时瞬时）。
4. ~~语法高亮用 chroma github 系配色（风险 3）~~（2026-10-06 失效：chroma 依赖已随 kit/markdown 删除，高亮由 MujicaUI MarkdownView/CodeBlock 内置词法高亮承担）。
5. 滚动条样式取 mygo 原生 theme（宽度 14 对齐，thumb 圆角/最小长以引擎实现为准）。
6. ~~用户长文折叠底部渐隐用裁剪近似（无 mask API）~~（2026-10-06 失效：用户气泡改用 chat.MessageBubble，长文折叠机制整体移除）。
7. 字体渲染随平台文字引擎（Core Text/DirectWrite/Pango），非浏览器字体栈（notes §7）。
8. **发送/停止钮状态机**：ZCode 为「运行中且有草稿 → 仍显示发送（入队语义），`showStopControl = canStop && !hasDraftToSubmit`」（zcode-chat-specs §2.2）；v1 无队列，运行中一律显示停止、发送禁用（草稿保留，Turn 结束恢复可用）。
9. **权限对话框默认键**：Enter=允许（对齐 ZCode ConfirmDialog「Enter=确认」的平台习惯，`ConfirmDialog.tsx:150-190`）；fail-closed 由工具 `Effect` 分类承担（只读才放行），对话框默认值不承担安全职责；如需更保守可对调 DefaultButton（§5.7）。
10. **右键/下拉菜单为系统原生样式**：mygo `ContextMenu`/`MenuButton` 由系统菜单渲染（`ui/menu.go:10-13`），无逐项样式钩子——ZCode 的 bg-menu/rounded-md/hover 自绘观感不做，语义（项/分隔/子菜单/Chosen）保留；自绘菜单 V1.1 评估。
11. **Enter 发送的帧时差分边界**：组合态回车依赖「insertText 先于 editKey 入队」（`surface_input.go:126-135`）使缓冲差分不等于单个 `\n`；罕见场景（IME 以空提交收尾组合后回车）可能误判为发送；headless 无法完整建模真机 IME 排序（`tt.Compose`+直接 `Key(Enter)` 会丢弃 marked 文本，`editor.go:688-691`），该场景以 W4 真机验收覆盖。

---

## 11. 最初方案会话的历史验证记录

实际读取：根 `AGENTS.md`（环境注入）；四份规格文档全文（`docs/go-migration/{mygo-ui-notes,zcode-tokens,zcode-chat-specs,zcode-shell-specs}.md`）；mygo 源码 `ipc.go`、`ui/window.go`、`ui/context.go`、`ui/theme.go`、`ui/richtext.go`、`ui/list.go`、`ui/editor.go`、`ui/handler.go`、`ui/svg.go`、`ui/toast.go`、`dialog.go`、`modules.go`、`content.go`、`ui/doc.go`、`examples/gallery/main.go`、`examples/native/main.go`、`go.mod`、`LICENSE`；旧栈 `core/model/src/{provider,stream,message,tool}.rs`、`core/agent/src/{tool,event,runner}.rs`、`core/tools/src/{lib,filesystem,command,search}.rs`、`core/provider/src/config.rs`、`apps/desktop/src/{storage,providers,app_settings}.rs`。

实际执行的检查（结论均来自真实输出；**复审修订轮新增**见本节末尾）：
- `ls /Users/chengliang/code-projects/mygo` + `cat go.mod`：module `github.com/egoist/mygo`，go 1.27.1，MIT LICENSE。✅
- `rg` 定位并逐段 `Read`：`mygo.Bind`（ipc.go:90-104，webview IPC 用）、`ui.View/WindowOptions/StateKey`（ui/window.go:18,31 / window.go:64,152-159,287）、`Theme 结构/LightTheme/DarkTheme`（ui/theme.go:7,87,113）、`Theme.SetSource/ThemeDark|Light|System`（modules.go:214-216）、`Toast/ToastAction`（toast.go:42,53）、`Span 字段`（richtext.go:15-33）、`ListState{Key,FollowEnd}`（list.go:38-49）、`List`（list.go:249）、`TextArea/TextInput`（editor.go:718,721）、`HandleInput/InputEvent/InputCompose`（handler.go:17,40-59,73）、`Dialog.Open{Directory}/Message{Buttons}`（dialog.go:24-44,75-89,118,181）、`Clipboard`（mygo_test.go:1438-1443）、`Window.Update/Invalidate`（content.go:148,128）、`MustParseSVG/Icon`（svg.go:51,81）、gallery 的窗口+对话框用法（examples/gallery/main.go:27-31,1447-1448,1471-1477）。✅
- 旧栈对照：`ModelProvider`（provider.rs:79-97）、`ModelStreamEvent` 全枚举（stream.rs:17-100）、`Message/ContentBlock/MessageRole`（message.rs:9,165,229）、`ToolDefinition` 校验（tool.rs:21-86）、`AgentTool` trait（agent/tool.rs:1062-1105）、`ToolRegistry`（agent/tool.rs:1110-1160）、`TurnRequest`（runner.rs:280-347）、`AgentStreamEvent` 信封（event.rs:484-532）、工具名 Read/Edit/Write/Bash/Glob/Grep（filesystem.rs:58,129,195 / command.rs:68 / search.rs:48,118）、`#` URL 语义全文（providers.rs:1298-1340）、数据根 `~/.keencode`（storage.rs:17-36）、AppSettings（app_settings.rs:90）。✅
- `go version` → go1.26.5 darwin/amd64（低于 mygo 要求的 1.27.1，走 GOTOOLCHAIN 自动拉取，风险 7）。✅

**第三轮复审修订实际执行的检查**：
- B1/B2 复现与修复验证：在 `/tmp/edtest` 建模块（replace 本地 mygo，GOTOOLCHAIN=auto）写 headless 用例，`go test -count=1` —— 朴素「拦 InputKeyDown+KeyEnter」谓词 `sent=1`（**复现组合态误发送**）；「fn 内组合态跟踪」谓词同样 `sent=1`（`input.go:688-694` 实证 InputCompose 不投带内建编辑器元素的 fn）；**帧时缓冲差分方案三用例全过**（普通回车发送 / Shift+Enter 换行不发送 / `Compose+Type` 提交后回车发送）。期间实测 `ui.NewTester` 需显式宽高、无焦点不投键；`ev.Mods.Shift` 编译报 "has no field or method Shift"（改 `ev.Mods&ui.Shift` 位运算后过）。✅
- 源码实读：`internal/darwin/surface.go:700-709`（keyDown 先发 KeyPressed 再 `interpretKeyEvents:`）、`internal/darwin/surface_input.go:126-135`（先 TextComposition 后 TextInput）、`ui/input.go:53,487-495,676-694`、`ui/editor.go:485-491,688-691,726-733`、`ui/base.go:413-422`（TextInputBase/TextAreaBase）、`ui/keys.go:99-110`（Modifiers 位掩码+包级常量）、`ui/theme.go:30-45`（Scrollbar Color + ScrollbarWidth 平级）、`ui/menu.go:10-16,129-131`（`m.Item(label string)`、系统原生渲染）、`ui/widgets.go:549-553`（Tooltip 只收 string）、`ui/headless.go:353-361`（Compose 语义）。✅
- B5 实证：`cd /Users/chengliang/code-projects/mygo && git tag`（止于 v0.2.9）、`git log --oneline -3 459b512`（"Release 0.2.10"，未打 tag）；`git ls-remote --tags origin` 在本沙箱无网络输出，以评审方同命令结果与本地 tag 互证。✅
- S6 实证：`curl chroma LICENSE`（master/main 均 404）→ GitHub license API：文件名 COPYING、`spdx_id: NOASSERTION`；拉取内容解码——首段为 MIT（"Copyright (C) 2017 Alec Thomas / Permission is hereby granted…"），同文件内嵌字体等第三方数据许可（SIL OFL 等）。文档按此如实记录。✅

未验证项显式声明：①**版本化依赖在 CI 的可拉性未验证**（本沙箱 `git ls-remote`/proxy.golang.org 不可达；W0 首日以真实网络验证 `go get github.com/egoist/mygo@459b512` 伪版本并锁定 go.sum）；②**真机窗口验收未做**——所有界面结论以源码实读 + headless 测试为据，像素对照与 IME 真机行为留 W4；③chroma 全量 lexer 的二进制增量 ~10MB 为估计值，W2 实测。除此之外：本任务为方案规划，未创建正式 Go 模块、未编译骨架（除上述 /tmp 脚手架的 headless 机制验证）；W0 首个动作即建 `go.mod` 并以 notes §1.3 已验证的最小骨架冒烟。文档中标注"实测/✅"的条目均转引自 `mygo-ui-notes.md` 中上一会话已真实执行的命令，本次未重复执行。

**复审修订轮（第二轮）实际执行的检查**：
- `rg -n "InputKey\b" /Users/chengliang/code-projects/mygo/ui/*.go` → **无命中**（exit 1）；`sed -n '1,30p' ui/handler.go` 实读 `InputKind` 常量表 = `InputKeyDown/InputKeyUp/InputText/InputCompose/InputCommand/InputPointerDown|Up/...`（handler.go:5-30）——证实原稿 `InputKey` 不存在，已全文改为 `InputKeyDown`。✅
- `sed -n '87,103p' core/model/src/usage.rs` → StopReason 实为 Completed/ToolUse/MaxOutputTokens/ContentFilter/Cancelled/Other{reason}，已补 `StopContentFilter` 与未识别原因→EventError 的归一策略。✅
- `sed -n '7,20p' core/model/src/message.rs` → `MessageRole::Tool` 存在（message.rs:18-19），Go 侧合并规则与两适配器拆装规则已写入 §5.1/§5.2。✅
- `sed -n '1333,1372p' apps/desktop/src/providers.rs` → `validate_exact_endpoint`（# 结尾必须以协议端点后缀结尾，否则 bail）与 `model_catalog_endpoint`（剥离 # 与后缀）实证，已补进 §5.6。✅
- `sed -n '39,53p' core/model/src/stream.rs` → `ReasoningSummaryDelta`/`ReasoningContinuation`（opaque 续传状态）实证，已补两个流事件与 Loop 落历史规则。✅
- ZCode `styles.css` 核对：`rg -n "icon-blue|terminal-bright-blue"` → `--color-icon-blue: var(--color-terminal-bright-blue)`（:162）；`.theme-zai-light` 块内 `#0066dd`（:518）、`.theme-zai-dark` 块内 `#80beff`（:663）（块边界 :461/:606 已 `sed` 实读确认）——Palette IconBlue 取 zai 值，默认主题值 #00A6F4/#0084D1 仅备查。✅
- `sed -n '236,250p' mygo/ui/list.go` → `Justify(End)` 语义="把不满一屏的行沉到底部"（list.go:243），非居中——§4.2 chatlist.go 已改为草稿态弹性留白居中与沉底分离。✅
- `sed -n '270,285p' mygo/ui/context.go` + `rg/sed ui/scope.go:152-160` → `c.Shortcut(mods,key)` 在无 modal 或上下文处于当前 modal 内时生效、被遮挡上下文返回 false——Esc 停止归属 `app/chat.go`（§6 第 4 条）。✅
- `curl -s https://raw.githubusercontent.com/yuin/goldmark/master/LICENSE | head -3` → "MIT License / Copyright (c) 2019 Yusuke Inuzuka"——原稿 BSD-2 有误已改。✅
- `go doc encoding/json/v2`（本机 go1.26.5）→ "doc: cannot find package"——证实 json/v2 默认不可用，journal 改用 `encoding/json` v1 并在 go.mod 锁 go 1.27.1。✅
- 文档自检：`rg -n "InputKey\b|BSD-2|zaiDark/zaiDark|三分支|replace 指本地检出|三态|leading-1.75|1.625"` 复核无残留矛盾表述（InputKey/1.625 仅以"不存在/不使用"的澄清语境出现）。✅


## 12. 2026-10-06 实施与验收更新

当前以 `docs/go-migration/HANDOFF.md`、`acceptance-2026-10-06.md` 与 `workbench-2026-10-06.md` 为准。上文保留最初设计决策与历史证据，不代表当前全量验收完成。

- 本日追加（聊天/Agent 组件官方化）：应作者发布的组件分组（https://mujicaui.zacharyzhang.com/#components ），C 流自研渲染整体替换为 MujicaUI 官方组件（`github.com/ZacharyZhang-NY/MujicaUI`）：`chat.MessageList`/`MessageBubble`/`MarkdownView`/`ThinkingBlock`/`TypingIndicator`/`ScrollToBottomButton`（MessageList 内置）与 `agent.ToolCallCard`；删除 kit 的 `markdown.go`/`inline.go`/`codeblock.go`/`toolcard.go` 与 `kit/markdown/` 子包及 chroma 依赖。错误横幅与草稿问候分支保留自研（理由见 §4.2 C）。每帧 `core.Use` → `theme.Apply` 顺序进 `App.View`；聊天区外观随之切为 MujicaUI 官方酒红主题（与 zai 外壳并存的取舍，后续可用 `core.Settings` 令牌覆盖统一）。差异清单 1/3/4/6 随之失效。

- 本日追加：基础三栏与右侧真实终端/Git差异/文件浏览已完成原生验收；左侧只保留新建对话、置顶、项目、对话、底部设置。终端按最新用户要求放右栏，覆盖旧计划“下方终端/V2评估”的范围判断。新增Darwin PTY补丁，与原resize/IME补丁均保存在patches；依赖发布版本仍未闭环。

- 已实现 Go + mygo 原生首版；生产装配的 HTTP/SSE→Agent Loop→六工具→journal→UI 闭环测试覆盖 Messages 和 Chat Completions。权限拒绝、HTTP 错误详情与 Esc 取消另有用例。
- macOS 原生脚本已通过真实模型回复、实际 Write 文件、权限弹窗、重启恢复、亮暗主题、Shift+Enter、中部 Enter，以及搜狗拼音组合态提交。脚本读供应商配置到临时目录，不改原配置。
- 新修复包括 canonical 工作区路径校验、失效模型选择、供应商保存失败的内存状态污染、气泡正文层级、输入壳内工具条、长模型菜单滚动、占位文本行高、侧栏新建入口对齐与主面板边框/inset。
- `go.mod` 目前仍用本地 mygo replace（基线 `1ff0c41aeea303df5f948dfdfc4e6bbeb6a46df0`），不是 W0 原计划的可移植锁定版本。macOS resize/IME 修复保存在 `docs/go-migration/patches/`；换机器需应用补丁。可发布依赖版本与 CI 可拉取性仍未闭环。
- 修正规格：会话主面板使用 `Background`，不是 `Panel`；暗色主区为 `#161616`。侧栏 macOS vibrancy 仍未接入，截图与 ZCode 仍明显不同。
- 新增待对齐项：编辑器默认至少三行（60 DIP），ZCode 最小40；问候字号固定30而非20–30自适应；问候语时间分段、Logo、建议入口、composer 项目/权限行与模型分组仍未完成。菜单滚动与键盘选择已补齐。
- 原差异11不能再单独用帧时差分保证 IME：macOS 补丁让组合态按键先归输入法处理，避免编辑器抢走回车/导航/删除键；Windows 输入法与其他 macOS 输入法未验收。
- 已取得安装版 ZCode 3.14.4 的原生截图作外壳对照，但它不是所固定源码提交的重建产物，且会话/模型状态有差异；没有宣称全屏像素一致或 W4 全绿。性能预算、Windows 原生验收与后续功能继续待验。

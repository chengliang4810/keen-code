# MyGo 原生 UI 调研笔记（面向 ZCode 风格聊天桌面应用）

> 调研对象：`/Users/chengliang/code-projects/mygo`（module `github.com/egoist/mygo`，go 1.27.1，版本 0.2.10，`mygo.go:70`）。
> 所有结论均出自本次实际阅读的源码/文档，引用格式 `文件:行号`（相对仓库根）；带 ✅ 的命令是本次真实执行并通过的。
> 结论先行：**用 mygo 的 `ui` 包做无 webview 的聊天类桌面应用完全可行**：flexbox/grid 布局、段落级虚拟化、富文本混排（字号/颜色/粗细/等宽可混排）、IME 输入、无窗口测试、亮暗主题、SVG 图标都是现成的；需要自建的只有 Markdown 解析层。

---

## 1. 模块导入路径、replace 写法与最小可编译骨架

### 1.1 导入路径

```go
import (
    "github.com/egoist/mygo"    // 桌面框架：App/Window/Dialog/Clipboard/Theme...
    "github.com/egoist/mygo/ui" // 自绘 UI 工具包（Content 的实现方）
)
```

`ui.View(fn)` 返回 `*ui.Content`，塞进 `mygo.WindowOptions.Content`（`ui/window.go:36`，`window.go:200-206` 的 `Content` 字段注释：有 Content 的窗口没有 Page，`URL`/`Page` 被忽略）。窗口渲染后端：macOS Metal、Windows Direct3D 11、Linux OpenGL/GTK3，纯 Go 无 cgo（`docs/ui/README.md:6-8`，`docs/README.md:145-156`）。

### 1.2 外部应用引用本仓库源码的写法

`mygo init -template native` 生成的项目直接 require GitHub 版本；要用本地检出的 mygo，CLI 自带 `-mygo` 参数写 replace（`cmd/mygo/init.go:54`、`init.go:169-173`：`gomod += "\nreplace github.com/egoist/mygo => %s\n"`）。手工写等价于：

```go.mod
module zcode

go 1.27.1

require (
	github.com/ebitengine/purego v0.11.1
	github.com/go-text/typesetting v0.3.5
	github.com/egoist/mygo v0.0.0
	golang.org/x/image v0.46.0
)

replace github.com/egoist/mygo => /Users/chengliang/code-projects/mygo
```

依赖清单必须与 mygo 的 `go.mod`/`go.sum` 一致（可直接拷贝它的 `go.sum`；缺 sum 条目会在 build 时报 `missing go.sum entry`，本次踩过）。

### 1.3 最小可编译 main（✅ 已在外部模块编译验证）

骨架（`examples/counter-native/main.go:44-57` 同款）：

```go
package main

import (
	"log"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

type app struct{ n int } // 状态放自己的类型里

func (a *app) view(c *ui.Context) {
	ui.Column(c).Fill().Center().Gap(16).Children(func() {
		ui.Textf(c, "%d", a.n).FontSize(40).Bold()
		if ui.PrimaryButton(c, "+").Clicked() {
			a.n++
		}
	})
}

func main() {
	a := &app{}
	mygo.App.WhenReady(func() {
		mygo.NewWindow(mygo.WindowOptions{
			Title:   "ZCode",
			Width:   980,
			Height:  680,
			StateKey: "main", // 记住上次窗口位置/大小（window.go:152-157）
			Content: ui.View(a.view),
		})
	})
	if err := mygo.App.Run(); err != nil {
		log.Fatal(err)
	}
}
```

验证命令与结果（本机，离线环境用缓存的 go1.27.1 工具链）：

- ✅ `go build -o zcode-skel .` → `BUILD_OK`，产出 Mach-O 64 位可执行文件，**15 MB**（远小于 50MB 预算）。
- ✅ `go vet .` → `VET_OK`。

### 1.4 线程模型（必须遵守）

- 包初始化时 `runtime.LockOSThread()`（`mygo.go:72-76`），**`App.Run` 必须从 main goroutine 调用**；除此之外本包几乎所有函数/方法可从任意 goroutine 调（跨线程调用会被转发到主线程并等待，`mygo.go:44-49`）。
- 事件监听器（OnClose/OnFocus…）在主线程跑，要快进快出；慢活开 goroutine（`mygo.go:57-59`）。
- `App.Run` 之前调用需要运行中 app 的东西（剪贴板、对话框等）会 panic（`mygo.go:52-55`）。
- 从其他 goroutine 改状态必须经 `win.Update(fn)`（主线程执行 fn 后重绘，`content.go:138-153`）或 `win.Invalidate()`（`content.go:128-136`）。

---

## 2. 视图模型：`func(c *ui.Context)` 立即模式 + 保留状态

### 2.1 心智模型

视图是 **"状态 → 界面" 的函数**，MyGo 在主线程上为每一帧调用它；帧之后再问元素"刚才发生了什么"（`docs/ui/views.md:1-8`，`ui/doc.go:17-24`）。要点：

- **元素只活在当前帧**（`ui/element.go:192`："An element only lives during the frame that built it"）。跨帧持久的东西全是你自己的状态结构体 + MyGo 替你保管的元素状态（焦点、滚动、正在编辑的文本、动画），后者按"元素在兄弟中的位置"或 `Key` 归属（`context.go:303-394` 的 `state` 结构）。
- **事件是提问**：`if ui.Button(c,"Delete").Clicked() { ... }` 处理代码就写在构建按钮的地方（`docs/ui/views.md:49-58`）。处理器在视图构建过程中改了状态 → MyGo 立刻重build这一帧（views.md:60-62）。
- 状态结构体在 main 里 `app := &app{}` 建一次，`router` 这类长命对象必须是状态字段、**绝不能在 view 里建**（views.md:44-46）。
- 一个 `Content` 可供多个窗口复用，各自有独立元素状态（`ui/window.go:30-34` 注释）。

### 2.2 重绘触发

| 方式 | 出处 |
| --- | --- |
| 输入事件后自动重绘 | `docs/ui/views.md:3-6` |
| `c.Invalidate()`（任意 goroutine 可调） | `context.go:238-239` |
| `c.AnimationFrame()`（连续帧，动画中每帧都调） | `context.go:242-245` |
| `c.After(d)`（延时一帧，时钟/光标闪烁用它） | `context.go:248-249` |
| `win.Update(fn)` / `win.Invalidate()`（跨 goroutine） | `content.go:128-153` |
| 动画值 `Animate/Loop` 内部自动请求帧 | `widgets.go:50-107` |

空闲窗口不绘制，2 秒后释放 CPU 帧（`docs/ui/rendering.md:48-50`）——符合"无任务不轮询"。

### 2.3 事件处理 API（`Element` 方法）

| 方法 | 含义 | 出处 |
| --- | --- | --- |
| `Clicked()` | 主键点击或焦点上按 Enter/Space | `input.go:759-766` |
| `DoubleClicked/RightClicked/Clicks/ClickModifiers` | 双击/右键/点击数/修饰键 | `input.go:770-805` |
| `Hovered/Pressed` | 悬停/按住 | `input.go:808-830` |
| `Dragged()` | 按住期间指针位移 | `input.go:895-905` |
| `Focused/FocusVisible/FocusWithin` | 焦点状态 | `input.go:833-852` |
| `Focus()/AutoFocus()` | 抢焦点（每帧调=保持）/出现时抢一次 | `input.go:856-873` |
| `Shortcut(mods,key)` | 元素或后代有焦点时的快捷键 | `input.go:878-880` |
| `c.Shortcut(mods,key)` | 全窗口快捷键（对话框后面不生效） | `context.go:277-282` |
| `Changed()/Submitted()` | 控件值变化 / 单行输入按了回车 | `input.go:908-911` |
| `PointerPosition/DroppedFiles/FileDragOver` | 指针位置/文件拖放 | `input.go:885-891`、`drop.go:11-21` |
| `HandleInput(fn)` | 逐键原始输入（终端类组件用） | `handler.go:73` |
| `ContextMenu(fn)` / `.Menu(fn)` | 右键菜单 / 菜单按钮 | `menu.go:31,51` |

键盘常量：`ui.KeyEnter/KeyEscape/KeyA…KeyF12`、修饰键 `Shift/Ctrl/Alt/Super`，`ui.Cmd` = macOS 上 Cmd、其他平台 Ctrl（`keys.go:99-113`）。窗口级快捷键示例：`c.Shortcut(ui.Cmd, ui.KeyS)`（`docs/ui/input.md:59-65`）。

### 2.4 Key 与元素身份

兄弟顺序会变的列表项要给容器 `Key`，否则状态按位置错位；**在创建时就消费输入的控件（Checkbox/Radio/Select/TextInput/List…）上加 Key 会 panic**，Key 要加在包住它们的元素上（`element.go:465-480`）。测试器对重复 Key 直接 panic（`headless.go:103-107`）。

---

## 3. 布局系统：flexbox + grid（DIP 单位）

容器：`ui.Column`（纵向、子元素拉伸到宽）、`ui.Box`（同 Column）、`ui.Row`（横向、垂直居中）、`ui.Grid`；`Reverse/Wrap/WrapReverse`（`element.go:483-504`，`docs/ui/layout.md:16-29`）。

常用 API（`element.go`）：

- 尺寸：`Width/Height/Size`（559-565）、`WidthPercent/HeightPercent/Fill/FillWidth/FillHeight`（568-580）、`Min/MaxWidth/Height`（583-586）、`AspectRatio`（696）。
- flex：`Grow(f)`（=flex:1，会把 basis 归零，适合"列表填满剩余空间"，600-606）、`Shrink`（610）、`Basis`（614）。
- 间距：`Gap/GapX/GapY`（508-514）、`Padding/Margin` 支持 CSS 缩写 1/2/4 个参数（524-556）；`ui.Auto` 自动 margin（`element.go:44-49`）：`Margin(0, ui.Auto)` 水平居中、`Margin(0,0,0,ui.Auto)` 推到行尾。
- 对齐：`Justify/AlignItems/AlignSelf/Center/AlignContent`（621-632），`Align` 常量 `Start/Center/End/Stretch/SpaceBetween/SpaceAround/SpaceEvenly`（`element.go:13-28`）。
- 定位：`Absolute()+Top/Right/Bottom/Left`（637-653，流内元素上是相对位移——badge 上浮用法 `layout.md:70-76`）；`Attach(at, self)` 锚点贴附（682-693）。
- 裁剪/可见性：`Clip/ClipX/ClipY/Invisible/Debug`（698-713）。

### Grid（`ui/grid.go`）

```go
ui.Grid(c).Columns(3).Gap(12)                    // n 等分列（61-67）
ui.Grid(c).ColumnTracks(ui.Fixed(220), ui.Fr(1)) // 侧栏+主区（78-84）
// Track: ui.Fixed(v) / ui.Fr(f) / ui.FitContent()（grid.go:18-30）
// ColumnStart/RowStart/ColumnSpan/RowSpan（102-115，负数=跨到末尾）
```

### 侧栏 + 主区（ZCode 主界面）两种写法

flex 版（gallery 的真实写法，`examples/gallery/main.go:301-306`）：

```go
ui.Row(c).Fill().AlignItems(ui.Stretch).Children(func() {
    app.sidebar(c)                        // Width(200).Shrink(0)
    ui.Column(c).Grow(1).MinWidth(0)      // MinWidth(0) 防止内容撑破！
})
```

grid 版（`docs/ui/grid.md` 推荐）：`ui.Grid(c).Fill().ColumnTracks(ui.Fixed(220), ui.Fr(1)).RowTracks(ui.FitContent())`，两段 `FillHeight()`。

分隔线、可拖拽分割：`ui.Divider`（`widgets.go:37-44`）、`Dividers(width,color)`（`element.go:773-779`）、`ui.Split(c, &size, first, second)`（`split.go:9`，gallery main.go:693-704）。

---

## 4. 控件清单与构造方式

无主题外观的底层叫 Base（`ui/base.go`），带主题外观的叫成品。所有控件吃 `t.Space(n)`（Spacing 单位，默认 4，`theme.go:55-61`）。

| 控件 | 构造 | 出处 |
| --- | --- | --- |
| 文本 | `ui.Text(c, s)` / `ui.Textf(c, format, args...)`，链式 `.FontSize().Bold().TextColor()...` | `widgets.go:21-30` |
| 按钮 | `ui.Button` / `ui.PrimaryButton`（`.Clicked()` 判定；空 label + `.Children()` 自定义内容） | `widgets.go:161-207` |
| 单行输入 | `ui.TextInput(c, &v).Placeholder("...").Submitted()`；`.Password()` | `editor.go:717-732` |
| **多行输入** | `ui.TextArea(c, &v)`：撤销/选区/剪贴板/IME/按段虚拟化全带 | `editor.go:720-721`、`textarea.go` |
| 无外观输入底座 | `TextInputBase/TextAreaBase` | `base.go:415-422` |
| 复选/开关/单选 | `ui.Checkbox(c,&b,label)`、`ui.Switch(c,&b)`、`ui.Radio[T](c,&sel,v,label)`、`CheckboxGroup` | `widgets.go:237-317`、`feedback.go:24` |
| 下拉 | `ui.Select(c,&sel,[]string{...})` | `widgets.go:626-650` |
| 下拉底座 | `SelectBase[T]`（Trigger/Popup/Item，自绘样式） | `base.go:254-358` |
| 组合框/自动完成/搜索/标签输入 | `ui.Combobox`、`ui.Autocomplete`、`ui.SearchField`、`ui.TokenField` | `combobox.go:298,343,378,436` |
| 菜单按钮/右键菜单 | `ui.MenuButton(c,label,build)`、`e.ContextMenu(build)`；`m.Item().Checked().Disabled().Shortcut().Chosen()`、`m.Separator()`、`m.Submenu()` | `widgets.go:673-682`、`menu.go:31,128-185` |
| 对话框 | `ui.Modal(c,&open,fn)`（遮罩+面板）、`ui.AlertDialog(c,&open,title,msg,buttons...) int`、底座 `DialogBase` | `widgets.go:592-603`、`feedback.go:160-210`、`base.go:394-413` |
| 弹层 | `ui.Popover(c,anchor,&open,fn)`、底座 `PopoverBase`；`ui.Overlay(c,fn)` 自由层 | `widgets.go:607-616`、`base.go:366-387`、`widgets.go:583-588` |
| Tooltip | `.Tooltip("文本")`（悬停 600ms 后出现，自动防溢出） | `widgets.go:549-579` |
| 滚动容器 | `ui.Scroll`（纵向）/`ScrollHorizontal`/`ScrollBoth`；`.TrackScroll(&state)` 持久滚动位置；`.ScrollIntoView()` | `widgets.go:478-498`、`scroll.go:7-66` |
| **虚拟化列表** | `ui.List(c, *ListState, n, rowFn)`：只构建可见行±2，行高可异构，估算未测行高 | `list.go:249-257`、`list.go:23-33` |
| 列表状态 | `ListState{Key,FollowEnd,Selected,Selection,Label,Header,Reorder}` + `ScrollTo/ScrollIntoView/ScrollToEnd/Visible/AtEnd` | `list.go:38-188` |
| 表格/树/大纲/网格视图 | `ui.Table`、`ui.Tree/TreeItem`、`ui.Outline/OutlineTable[K]`、`ui.GridView`（同用 ListState） | `table.go:142`、`tree.go:26,44`、`outline.go:101,128`、`gridview.go:87` |
| 标签页 | `ui.Tabs(c,&sel,labels...)`；底座 `TabsBase/Tab` | `tabs.go:15`、`base.go:163-209` |
| 侧栏 | `ui.Sidebar/SidebarSection/SidebarItem`（带 Badge、箭头动画、键盘选择） | `sidebar.go:41,103,162` |
| SVG 图标 | `icon := ui.MustParseSVG(embed字节)`（包级解析一次）→ `ui.Icon(c, icon)`（随 `FontSize`/`TextColor` 着色）；`ui.Image(c, iconSVG)` 按原色显示；`.Rotate(deg)` | `svg.go:40-101` |
| 分割线/徽标 | `ui.Divider(c)`、`ui.Badge(c, "3")` | `widgets.go:37`、`sidebar.go:219-225` |
| 其他 | `Slider/StepSlider/RangeSlider/Stepper/NumberInput/Progress/Spinner/Meter/Rating/Avatar/ColorWell/DateInput/TimeInput/Calendar/Link/Toolbar/Toggle/ToggleGroup/Segmented/Breadcrumbs/Collapsible/Accordion/EditableText/FindBar` | `widgets.go:320-474`、`indicators.go`、`colorpicker.go:249`、`date.go`、`timeinput.go:18`、`toggle.go`、`toolbar.go:31`、`collapsible.go:80,147`、`editable.go:38`、`feedback.go:222` |
| 自绘 | `.Draw(func(p *Painter, r Rect))` / `.DrawOver(...)`；`p.Fill/Stroke/StrokePath/Shadow/Line/Text/Glyphs/RichText/MeasureText`；`ui.Path` | `element.go:993-996`、`paint.go:765-814`、`gallery main.go:498-516` |
| 拖放 | `.Drag(&v)` / `ui.Drop[T](e)` / `ui.DragOver[T](e)`；文件拖放 `DroppedFiles/FileDragOver` | gallery main.go:563-566、`drop.go:11-21` |
| Toast | `c.Toast("Saved")`、`c.ToastAction(msg,"Undo",fn)` | `toast.go:42-55` |

**多行 Editor 细节（聊天输入框关注点）**：

- IME：组合中文本插入到光标处显示下划线、候选窗口定位到 `caretRect`；测试器可 `tt.Compose(text, caret)` 模拟（`editor.go:690-704`、`editor.go:665-677`、`headless.go:358-360`）。平台 IME 集成见 `docs/ui/input.md:84-89`。
- **回车行为**：单行 `TextInput` 的 Enter → `Submitted()`（`input.go:911`）；**`TextArea` 的 Enter 是插入换行**（`editor.go:486-488`），不会 Submitted。要做"Enter 发送、Shift+Enter 换行"，两个办法：a) 用单行 `TextInput`（自带 Submitted + submitMods 记录修饰键，`context.go:350-352`）；b) 用 `HandleInput` 拦截（`handler.go:73`）或在 view 里检查 `c.Shortcut`。✅ 本次骨架测试实证：TextArea 上 `tt.Key(0, ui.KeyEnter)` 后 `Submitted` 未触发、n 不变。
- 光标/选区/撤销：内置（`editor.go:69-102`），200 步撤销、逐词/逐行移动、macOS emacs 键、双击选词三击选行（`editor.go:574-601`）。
- 占位符在测试里 **Find 不到**（不是 Text 元素），给输入框 `.Label("Draft")` 才能被 `tt.Click("Draft")` 定位（`element.go:936-938`；✅ 本次实测：`Find("Say something…")` 返回 false）。

---

## 5. 富文本能力（关键结论：可行，且有三层 API）

### 5.1 同一段内混排样式：`ui.RichText` + `ui.Span` ✅

```go
ui.RichText(c,
    ui.Span{Text: "Saved "},
    ui.Span{Text: "report.pdf", Weight: 600},
    ui.Span{Text: " to "},
    ui.Span{Text: "~/Documents", Font: "monospace", Color: t.Accent, Underline: true},
    ui.Span{Text: "match", Background: ui.RGBA(250, 204, 21, 0.4)},
)
```

`Span` 字段：`Font`（字族列表）、`Size`、`Weight`、`Italic`、`Color`、`Underline/WavyUnderline/Strikethrough`、`DecorationColor/DecorationThickness`、`Background`、`LetterSpacing`、`Features`（OpenType 特性）（`richtext.go:15-33`）。零值继承段落基础样式（文本元素链上的 `FontSize/TextColor/Font` 等会作为段落默认，`richtext.go:35-45`）。**spans 作为一个段落整体折行**；`\n` 分段（`richtext.go:45-47`）。注意：Span 只管排版样式，**不能带交互**——交互用 5.2。

另有 Painter 级富文本（在 `Draw` 里手绘用）：`p.RichText(x,y,width,spans...) (w,h)`、`p.MeasureText(width, spans...)`、`c.MeasureText(...)`（构建期量尺寸，`richtext.go:212-232`）。

### 5.2 段内可交互元素（inline elements）✅

在 `Text/RichText` 的 `.Children(fn)` 里只能放**文本类元素**（`Text/Link/RichText`，放别的会 panic，`context.go:131-133`），它们延续同一段落折行，并保留各自的交互（`Clicked/Hovered/焦点/Tooltip/右键菜单`），命中区域跟随其文字跨行分布（`richtext.go:47-60`、`inline.go:1-16`、`inline.go:166-186` 用 `layout.Selection` 给每个 inline 元素算出逐行 frag）。

```go
ui.RichText(c).Children(func() {
    ui.Text(c, "Read ")
    ui.Link(c, "the guide", "https://example.com")  // 段内真链接，Tab 可达
    ui.Text(c, " or ")
    if ui.Text(c, "show an example").TextColor(t.Accent).Clicked() { ... }
})
```

这正是聊天消息里 "@提及"、"代码 span 可点击"、"行内链接" 的实现路径。**等宽混排**：inline `ui.Text(c, code).Font("monospace").TextBackground(...)` 或 Span{Font:"monospace"}（gallery main.go:801 已示范整段 monospace）。

### 5.3 长文档逐行/逐段渲染（虚拟化）✅

三种现成机制：

1. **`ui.List`**：`docs/ui/list.md` 全文就是按"聊天记录"设计的——异构行高按显示实测、未显示行高估算、`ListState.Key` 让行状态跟随消息 ID、`FollowEnd` 聊天尾部跟随（滚走即停、滚回即续）、`Header` 置顶日期戳、`Justify(ui.End)` 少量消息沉底、`ScrollToEnd/Visible/AtEnd` 做"跳到最新/向上加载更早"。gallery 有完整聊天示例：`examples/gallery/main.go:986-1059`（气泡用 `MaxWidthPercent(72)` + `Margin(0,0,0,ui.Auto)` 右对齐）。
2. **TextArea 内部**：文本按**段落**虚拟布局——只有视口内的段落真正排版，其余用估算高度，锚定段落保持视口稳定，离开视口 512 段后释放布局（`textarea.go:10-46`、`textarea.go:299-307`）。万行日志可直接塞 TextArea。
3. **普通 Text** 是整段排版（有内部缓存），适合单条消息；超长单段没有跨元素虚拟化——**分段交给 List**。

### 5.4 没有现成 Markdown 时的实现建议（mygo 不带 MD 解析）

- 解析层自建（或引黑盒 MD parser 库，只借它产出的 AST）。
- **块级 → `ui.List` 一行一块**（或普通 Column + `ui.Scroll`，文档不超大时够用）：paragraph/heading/list-block/code-block/quote/blockquote/hr/img 各给一个 rowFn 分支。
- **段内 → `ui.RichText`**：把 inline tokens（em/strong/code/link）转成 `[]ui.Span`；需要交互（链接点击、代码复制按钮）时改用 `RichText(c).Children(...)` 拼 `ui.Text` + `ui.Link`（5.2）。
- 代码块：`ui.Text(code).Font("monospace").NoWrap()`（`element.go:930` 不折行）放进 `ScrollBoth`（`widgets.go:494`），或逐行 `FixedLineHeight` 对齐行号（`element.go:849-853`）；高亮 = 把 token 转成 Span{Color}。
- 滚动位置保持：每条消息的 row 里嵌 `ui.Scroll().TrackScroll(&msgState.scroll)`（`scroll.go:22-30`）。

---

## 6. 主题与颜色

### 6.1 颜色 API（`ui/color.go`）

- `ui.RGB(r,g,b)`（:17）、`ui.RGBA(r,g,b, alpha0to1)`（:20）、**`ui.Hex("#2563eb")` 支持 #rgb/#rgba/#rrggbb/#rrgbbaa，解析失败 panic**（:24-30）→ 精确 hex 直接用。
- 运算：`c.Alpha(a)`（:84）、`c.Mix(other, t)`（:96）、`c.Over(base)`（:103）。

### 6.2 Theme（`ui/theme.go`）

- 结构：`Background/Surface/SurfaceHover/SurfacePressed/Border/Text/TextMuted/Accent/AccentHover/AccentPressed/AccentText/Danger/Warning/Success/Selection/Focus/Scrollbar(+Width)/Radius/Spacing/FontSize/Font/Dark`（:7-51）。
- `LightTheme()`/`DarkTheme()` 的完整默认值都在源码里（:87-137，如浅色 Accent=#2563eb、背景 #ffffff/#18181b），改 ZCode 色板时直接覆盖。
- **亮暗跟随系统**：每帧 `c.Theme()` 自动是当前外观；系统切换时引擎换主题并重绘（`context.go:82-103` reset 时取 `rt.defaultTheme()`；`ui/window.go:54-57` ThemeChanged 回调）。要固定外观用桌面层 `mygo.Theme.SetSource(mygo.ThemeDark|ThemeLight|ThemeSystem)`（`docs/native.md:116-128`，可在 Run 前调用）。
- **自定义主题**：改副本再 `c.SetTheme(&t)`（`context.go:205-209`）：

```go
t := *c.Theme()
t.Accent, t.Radius = ui.Hex("#7c3aed"), 8
c.SetTheme(&t)
```

- 桌面偏好：`c.Preferences()` 给系统强调色/高对比/文字缩放/减少动效，默认主题自动跟随（`docs/ui/styling.md:77-93`）。测试里 `tt.SetDark(true)`、`tt.SetPreferences(...)` 模拟（`headless.go:147-160`）。

---

## 7. 字体

- 默认字体栈：走**系统文本引擎**（macOS Core Text、Windows DirectWrite、Linux Pango），默认界面字体 SF/Segoe UI/GTK 桌面字体，系统字体自动 fallback 到其他文字与 emoji，RTL 自动（`docs/ui/text.md:81-89`）。默认字号 macOS 13、其他 14（`theme.go:79-84`）。
- **CJK（中文）**：无特殊配置——系统 fallback 链原生覆盖（text.md:83-84 "falling back to the system's fonts for other scripts and emoji as native apps do"）；gallery 明确展示了多文字混排 `English, Ελληνικά, Русский, 日本語, 한국어, العربية, עברי׽, हिन्दी 🎉`（gallery main.go:804）。逐平台亚像素渲染细节见 `docs/architecture.md:1540-1560`（文字与各平台原生应用逐像素一致）。
- 指定等宽：`.Font("monospace")` 是系统等宽字体（`element.go:830-834`、text.md:85）。
- **自带字体**：`//go:embed` + `ui.RegisterFont(data, "Inter")`（`ui/window.go:44-46`），然后 `.Font("Inter")` 或 `Theme.Font` 全局生效；字族列表 `"Inter, Noto Sans JP"` 逐族 fallback（text.md:88-102）。中文字体需要自带时：把含 CJK 的字体文件 embed 后注册并放进字族列表即可（渲染管线不改变）。
- 底层自绘文字（终端/画布内）：`ui.Shape(s, ui.Font{...}) []Glyph` + `p.Glyphs(...)`（`glyphs.go:66-79`、`glyphs.go:79`），不走元素布局，需自己缓存（"it caches nothing: keep the glyphs"）。

---

## 8. 无窗口视图测试与 go run 开发流程

### 8.1 `ui.NewTester`（`headless.go:93-109`）

软件渲染器在内存里出帧；`Click/Find/HasText/Texts/Key/Type/Compose/RightClick/Menu/ChooseMenuItem/Scroll/SetSize/SetScale/SetDark/SetPreferences/Image/Clipboard/OpenedURLs/Announcements/Focused`（`headless.go:128-417`、`docs/ui/testing.md`）。`tt.Image()` 可拿最后一帧的 `*image.RGBA` 做快照；`ui.Render(view, w, h, scale)` 一帧出图（`headless.go:84-88`）。

**范例测试文件（都在仓库内，✅ 本次全部跑过）**：

| 范例 | 覆盖 |
| --- | --- |
| `examples/counter-native/main_test.go:11-34` | 最小样板：点击按钮、断言状态与 HasText、方向键 |
| `cmd/mygo/template/native/main_test.go.tmpl` | init 模板自带的测试（点击、输入框打字） |
| `ui/menu_test.go:14-96` | 右键菜单：菜单项/勾选/子菜单/禁用项/快捷键展示 |
| `ui/select_test.go:9-60` | 双击选词、拖拽选择、Cmd+C 复制、只读文本 |
| `ui/list_test.go:14-55` | 虚拟列表几何断言（行盒、覆盖、gap）辅助函数 |
| `ui/textarea_test.go:23-45` | 文本 buffer 随机编辑不变量 |

外部应用同样能跑（✅ 本次在外部模块实测）：`go test -v -count=1 .` → PASS（骨架聊天视图：点 Send、给 TextArea 打字、断言 `a.draft=="hello"`、验证 Enter 在 TextArea 是换行不是提交）。

注意：`Find/Click` 按**文本或 Label** 定位（`headless.go:168-205`）；输入框内容不是 label，要 `.Label()`（见 §4）。Settle 最多跑 20 帧（`headless.go:112-120`）。测试模式里重复 Key 直接 panic（`headless.go:103-107`）。

### 8.2 开发/构建流程

```sh
# mygo 仓库内
go run ./examples/counter-native     # 直接跑（docs/ui/README.md:57-59）
go test ./ui                         # ✅ 全套 ui 测试：ok 34.975s（本次实测）

# 外部应用
go tool mygo dev                     # go.mod 以 tool 引入 mygo-cli 时；改 .go 热重建重启（docs/getting-started.md:94-105）
go test                              # 无窗口视图测试
go tool mygo build                   # 产物：macOS .app + dmg / Windows exe+installer / Linux 包（getting-started.md:183-203）
```

纯原生 UI 项目只需 Go，**不需要 Bun/node**（getting-started.md:75-92）；无 cgo，可交叉编译 `-platform darwin/universal,windows/amd64,linux/amd64`（getting-started.md:198-203）。运行环境要求：全原生 UI 的 app 在 Linux 只要 GTK3（不要 WebKitGTK）、Windows 不要 WebView2（`docs/README.md:147-156`）。

---

## 9. 坑清单（本次阅读源码 + 实测得出）

1. **线程**：`App.Run` 必须在 main goroutine（init 时已 LockOSThread，`mygo.go:72-76`）；goroutine 改状态必须 `win.Update`/`Invalidate`，否则是数据竞争且不重绘。
2. **视图里禁止建长命对象**：router、ListState 等放状态结构体字段；视图每帧重跑（views.md:44-46）。UI 里读到的 `*Element` 别存起来——下帧就失效（element.go:192）。
3. **Key 的 panic 语义**：给"创建时消费输入"的控件加 `Key` 直接 panic（element.go:474-477）；同父下重复 Key：生产环境打日志、Tester 里 panic（headless.go:103-107）。同一个 `ListState` 被两个 List 用同一帧展示也 panic（list.go:278-280）。
4. **TextArea 的 Enter ≠ 提交**（换行，`editor.go:486-488`）；`Submitted()` 只有单行有（input.go:911）。聊天输入框要么单行 + submitMods 区分 Shift+Enter，要么 HandleInput 自定义拦截。
5. **占位符/输入框内容测试找不到**：给输入控件 `.Label()`（✅ 实测 `Find("Say something…")` == false）。
6. **横向内容要 `MinWidth(0)`**：`Grow(1)` 的兄弟若内容过宽会撑破行（gallery main.go:304 主区就加了 `MinWidth(0)`）；`SingleLine()/MaxLines()` 防止长文本撑高。
7. **GPU 初始化是懒加载、可降级**：macOS 小改动走 CPU 直绘省电省内存（`ui/window.go:133-141`、rendering.md:10-15）；Linux 默认先 CPU、开销大才加载 OpenGL（Mesa 常驻约 50MB，rendering.md:17-29）；VM/WSL 无 GPU 时纯 CPU（约 1ms/整窗）。`MYGO_GPU=0|1` 强制、`MYGO_FRAME_STATS=1` 逐帧耗时日志（rendering.md:27-46）。虚拟机/远程桌面环境基本能跑，但性能预期按 CPU 渲染算。
8. **平台差异**：连续圆角仅 macOS（quarter circle elsewhere，styling.md:70-72）；字体渲染各平台不同但都贴近原生（architecture.md:1540-1560）；快捷键用 `ui.Cmd` 抽象（keys.go:112）；菜单按下即弹 macOS/Linux、抬起弹 Windows（menu.go:269-276）；Linux Wayland 全局快捷键要桌面门户（native.md:168-176）。系统级依赖：Linux GTK3、Windows 无额外、macOS 12+。
9. **对话框是阻塞 API**（`mygo.Dialog.*`），别在事件回调里直接调，开 goroutine（native.md:9-13）。
10. **Overlay 时机**：`ui.Overlay` 里的元素在视图返回后才挂到覆盖层（context.go:109-123）；popover 空间不足自动翻到上方（base.go:362-365）。对话框后的快捷键会被屏蔽（`Context.Shortcut` 的 `insideModal` 检查，context.go:277-282）。
11. **Tooltip/焦点细节**：Tooltip 600ms 延迟且指针按下时不显示（widgets.go:562-564）；`Focus()` 每帧调用=持续占有，`AutoFocus()` 只在首帧（input.go:856-873）；Tab 焦点环只在键盘导航时显示（FocusVisible）。
12. **`fmt` 等导入注意**：外部模块的 go.sum 必须含 mygo 全部依赖（✅ 实测缺 sum 报 `missing go.sum entry`）；本机若 Go < 1.27.1 需先 `go get` 工具链或用 `GOTOOLCHAIN`（mygo 要求 `go 1.27.1`，go.mod:3）。
13. **画布/坐标系**：所有尺寸是 DIP 浮点数，HiDPI 自动处理；自绘内 `p.Scale()` 拿像素比做像素对齐（glyphs.go:127-129）；`c.Size()` 拿窗口内容尺寸（context.go:212-213）；动画自绘里每帧调 `p.AnimationFrame()`（paint.go:61）或 `c.AnimationFrame()`。
14. **性能预期**（架构文档与 README 的实测口径）：无网页面进程、空闲 0% CPU（architecture.md:12）；空闲不渲染；二进制 ~15MB（✅ 本次实测）。列表 10,000 行滚动在 gallery 里就是默认演示项。

## 10. 对 ZCode 聊天应用的落地映射（速查）

- 主框架：`Row.Fill + 侧栏(Sidebar/Column.Width(240).Shrink(0)) + 主列(Grow(1).MinWidth(0))`，页面切换用 `ui.Router`（历史、页面状态保留、Cmd+[ ]，`router.go:56-128`）。
- 会话列表：`ui.List` + `ListState{Selected,Label}` 或 `ui.Sidebar`。
- 消息流：`ui.List` + `ListState{Key: msgID, FollowEnd: true, Header: 日期分隔}`（gallery 聊天例 1:1 对应）；消息体 RichText/inline children 渲染 Markdown；`Justify(ui.End)` + 空状态 Children。
- 输入区：`TextArea.Grow(1).Label("Input")` + 发送按钮；Enter 发送需要单行 TextInput 或 HandleInput 拦截；`ui.TokenField` 可做 @提及/模型选择 chips。
- 工具调用/终端：`HandleInput` + `Shape/Glyphs` 自绘或 `plugins/terminal`（`examples/terminal/main.go:23-39`，Ghostty vt，原生 UI 窗口直接嵌）。
- 设置页：`ui.Form/Field/Fieldset`（form.go:28,69,200）+ `Select/Checkbox/Switch`。
- 通知反馈：`c.Toast/ToastAction`、`mygo.NewNotification`（打包后）。
- 集成桌面：应用菜单/托盘/全局快捷键 API 同 webview 应用（`examples/native/main.go:154-201`），原生 UI 窗口完全通用；自定义标题栏用 `TitleBarStyle + c.TitleBar() + DragWindow()`（context.go:214-228、element.go:969-972、docs/ui/windows.md:20-37）。

## 11. kit A 流（基础控件）实现记录（`internal/ui/kit`，本会话补充）

以下为 W1 A 流实现 `internal/ui/kit` 基础控件时的实测发现与决策，供三流共同遵守；命令与结论均为本会话真实执行（`GOTOOLCHAIN=auto go test -count=1 ./internal/ui/kit/...` 全绿后记录）。

1. **`ui.Local` 泛型推断陷阱**：`Local[T](e, key, init func() T)` 以 init 的**返回值类型**推断 T——传 `func() *editorEnter` 得到 `**editorEnter`（编译报错才能发现）。正确写法 `ui.Local(e, key, func() editorEnter { return editorEnter{} })` 返回 `*editorEnter`。A 流 editor.go 与 B 流 tooltip.go 初版均踩中，见 `ui/context.go:287-301`。
2. **禁用态是引擎原生能力，不要手工再叠 50%**：paint 阶段对 `flagDisabled` 子树统一 `p.opacity *= 0.5`（`ui/paint.go:110-112`），等价 CSS `disabled:opacity-50`，且对「构建后链式 `.Disabled(true)`」同样生效（绘制发生在整帧构建之后）。A 流初版在 DrawOver 里再叠 50% 遮罩导致双重变暗（实测 138→110），已改为完全依赖原生透明度；Checkbox/Switch/SendButton 的手工半透明配色同理删除。像素断言：白底按钮禁用态 = `255*0.5+22*0.5` 四舍五入 **139**（zai-dark #161616 底）。
3. **悬停样式在构建期读取 `b.Hovered()` 即可**：`ButtonBase` 带 flagHover，指针移动跨入时 `setHover` 请求重绘（`ui/input.go:125-153`），构建期换脸即可跟随，无需（也无法从外部包）使用 unexported 的 `styleFn`。
4. **图标尺寸走 `FontSize`**：`ui.Icon` 的高度跟随字号（svg.go:71-88），kit `Icon(c, name, size)` 即 `ui.Icon(...).FontSize(size).Shrink(0)`（gallery main.go:1113 同款）。SVG 源为 lucide-static v1.52.0（ISC），17 枚 IconName + loader-circle + 派生的 square-fill（stop 按钮的 fill-current），逐文件保留上游 license 头。
5. **测试取元素矩形时区分「Label 在元素上」与「文本子元素」**：`tt.Find` 命中文本或 Label；按钮面探测必须用元素自身的 Label（文本子元素的 rect 含字形抗锯齿像素，会得到 25/37 这类假值）。居中文本的按钮尤其如此（`ButtonBase` 是 Center 的 Row）。
6. **Tester 默认亮色外观**：产品默认 zai-dark，测试需显式 `tt.SetDark(true)`，否则全部像素断言落到亮色值。
7. **公共文件改动（按 ask 记录）**：
   - `internal/ui/theme/palette.go` 新增 `DestructiveForeground` 字段（zai 两面均 `#ffffff`，`zcode-tokens.md` §2 destructive-foreground 行、styles.css:708 注释）：kit.go 契约的 `VariantDestructive`「白字」在 zai-dark 下不等于 `ForegroundInverse`（黑），Palette 原字段清单缺该令牌，kit 内禁 hex 字面量，故落在 theme 侧；`TestDestructiveForegroundToken` 双面断言。
   - 新增 `divider.go`（A 流第 9 个文件）：ask 明确要求「分割线」，且 §4.2 A 流 8 文件表无归属文件；不与 B/C 文件冲突。
8. **占位符颜色**：mygo 输入占位符固定画 `theme.TextMuted`（editor.go:906，桥接后 = ForegroundSubtle），ZCode 为 foreground-subtlest——无外部包钩子，记入有意差异（§10 清单的补充项）。
9. **Editor 帧时缓冲差分按 §4.2 落地全绿**：四序列（普通回车发送+清空 / Shift+回车换行不发送 / Compose+Type 提交后回车发送 / 朴素拦截谓词误发送回归）在 `TestEditorSequences`、`TestEditorNaivePredicateMisfires` 通过；判定/剥离实现与方案唯一偏差是剥离分支用 `strings.TrimSuffix(*draft, "\n")`，插入点在文本中部时的边界仍按差异 11 留给 W4 真机验收。

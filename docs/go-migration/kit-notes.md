# kit 分工实现笔记

各流（kitPartition，`docs/go-migration.md` §4）实现时的决策、与方案签名的偏差、
以及 mygo 行为备注记录在此，按流分节追加。公共文件 `kit/kit.go` 的冲突以方案
§4.1 定义为准。

## B 流：浮层（menu / select / popover / tooltip / dialog / toast）

实现文件：`internal/ui/kit/{menu,select,popover,tooltip,dialog,toast}.go` 及同名
`*_test.go`。只依赖 A 的导出契约（`kit.go`、`icon.go`）与 `ui/theme`，未改动
任何他人文件。

### 与 kit.go / 方案签名的偏差（均为本流自有 API，未动公共文件）

1. `ContextMenu(e *ui.Element, build func(m *ui.Menu))`：方案 §4.2 草图为
   `ContextMenu(c *ui.Context, build)`。挂载菜单必须有宿主元素，context 不承载
   菜单状态，故签名取元素；仍是 `(*ui.Element).ContextMenu` 的薄封装。
2. `Select[T ~string](c, sel, items, opts ...SelectOptions)`：方案草图无选项
   参数。变参 `SelectOptions{LG, Disabled bool}` 承载设置页 lg 档 trigger
   （zcode-shell-specs §3：lg 为 `h-8 rounded-lg pl-3 pr-2`）与禁用态；单项
   调用点形状与方案签名一致。
3. `Dialog`/`Confirm`/`Prompt` 统一返回 `DialogResult int`（`DialogNone=-1 /
   DialogCancel=0 / DialogConfirm=1`），方案对 Confirm/Prompt 写的是裸 `int`，
   归一成命名类型便于 Switch 分派。
4. 对话框页脚按钮走本流 `dlgButton`（未用 A 的 `kit.Button`）：ConfirmDialog
   规格要求按钮内嵌 `esc`/`⏎` 键位 chip（zcode-shell-specs §4.2，
   ConfirmDialog.tsx:150-190），`kit.Button(label)` 不收子元素。配色规则与
   button.go 一致（hover 按各 variant 的 CSS alpha，预合成于面板底色）。
5. Toast 拆成两个入口：`ShowToast(c, kind, msg)` 记录入栈（同文案替换、上限
   3 条），`Toasts(c)` 每帧渲染。原因：overlay 元素只在构建它的那一帧存活
   （mygo `ui/element.go:192`），toast 必须跨帧，栈状态放在 kit 包内，渲染
   契约是「app 根视图每帧最后调一次 Toasts(c)」（W3 落线）。

### mygo 行为备注（headless 实测）

- `c.After(d)` 只登记唤醒时间（mygo `ui/runtime.go:573-581` scheduleAt），不
  请求帧；headless 测试里延时路径要显式 `tt.Frame()` 推帧（原生 Tooltip 用例
  则 sleep 700ms 后 Frame）。
- `ui.SelectBase` 在构造时就在它**自建的** trigger 上消费 `Clicked()`；浮层
  trigger 必须样式化 `sel.Trigger` 本身（照 `ui.Select` 的做法），再造第二个
  ButtonBase 会收不到点击（本流第一版踩过，测试复现后修正）。
- `ui.Local[T]` 的 T 是值类型：`init` 返回 `tipState` 才得到 `*tipState`；
  写成返回 `*tipState` 会得到 `**tipState`。
- 闭包里创建元素才能挂对父级（`row.Children(func(){ in = ... })`）；Go 侧裸
  标识符语句 `func(){ in }` 直接编译错。
- 单行 `TextInputBase` 的 Enter 提交在 mygo 编辑器内无 IME 组合态守卫
  （`ui/editor.go` editKey 分支）；真实平台依赖「insertText 先于 editKey 入队」
  的队列顺序（`internal/darwin/surface_input.go:126-135`），残留的真机组合态
  回车语义归 W4 原生验收（方案风险 4）。headless 下 `Prompt` 用例只覆盖普通
  回车与 Esc 路径。

### 有意差异（对齐方案 §10 记录）

- 对话框遮罩 `black/60` 保 alpha、无 backdrop-blur；Toast 面板不透明
  `bg-toast`（ZCode 为 `bg-toast/60 + backdrop-blur-xl`）——差异 2。
- 右键/菜单为系统原生样式，无 bg-menu/rounded-md/hover 自绘观感——差异 10。
- Tip 自绘面板定位在目标下方，第二帧起按实测尺寸做水平钳制与上翻（mygo 的
  `keepInWindow` 不导出）；首帧按目标左下角原位出现。
- Confirm 紧凑壳不做 `top-[44%] min-h-[161px]` 的定位/最小高，仅保留
  `max-w 400 / p-5 gap-5 / h-9 按钮 / 键位 chip / min-w-28·32`。
- `shadow-md` 等双阴影近似为单层软阴影（mygo 每元素一层 Shadow）。

### 本会话验证记录（全部真实执行）

- `GOTOOLCHAIN=auto go build ./internal/ui/kit/...` → 通过。
- `GOTOOLCHAIN=auto go vet ./internal/ui/kit/` → 通过。
- `GOTOOLCHAIN=auto go test -count=1 ./internal/ui/kit/...` → **最终全绿**：
  `ok keencode/internal/ui/kit` + `ok keencode/internal/ui/kit/markdown`。其中
  B 流 15 个用例（menu×2、select×4、popover×1、tooltip×3、dialog×4、toast×1）
  全过；A 流（Button/Checkbox 等）与 C 流（markdown）的用例在各自完成前一度
  FAIL/编译错误，收尾时复查已自愈，非本流文件。
- 中途用 `go test -overlay`（把 A 未完成的 `kit_test.go` 映射为空桩）隔离验证
  过本流测试，未改动任何他人文件。
- 命令带 `GOTOOLCHAIN=auto`：go.mod 锁 go 1.27.1、本机默认 1.26.5（方案风险 7
  的既定做法）。

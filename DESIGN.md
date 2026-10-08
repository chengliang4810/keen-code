# KeenCode 原生设计系统

KeenCode 是一个以 GPUI 绘制的桌面工作台。设计系统采用“ZCode 固定视觉基线 + Ely
组件承载”：`ely-gpui-component` 提供主题、控件和尺寸令牌，业务实现位于
`apps/desktop/src/native_ui/`，而 ZCode 3.14.3 固定提交
`29628c9acdb81b703bbd4080c207a0e7ce5e276e` 定义布局、密度、对比度和交互的像素验收
基线。参考边界见 `docs/frontend-zcode-source.md`，实际主题和尺寸以
`apps/desktop/src/native_ui/style.rs` 为权威。

固定来源现在同时有同平台 Windows 参考证据：`out/ely-native/zcode-windows-reference-5/native-run/report.json`
绑定 `out/zcode-reference-3.14.3/ZCode.exe`（SHA-256
`afb17a861ce435e1ebecb32fa99ca14daa94adadc6054b5f2116b418248cace5`），其官方 v3.14.3
annotated tag peeled 到上述固定提交。参考运行使用 Dark、DPI `192`、逻辑窗口
`1280x820` 和物理 client `2560x1640`，最终截图为
`out/ely-native/zcode-windows-reference-5/native-run/screenshots/0017-workspace-final.bmp`，
metrics 为 `metrics/0018-workspace-final.json`。现有 dpr2 浏览器 PNG 仍只用于内容布局校准，
不能替代这份 Windows 原生参考或单独证明目标实现通过。

本文面向编写原生界面的 Agent，优先约束信息层级、状态表达和交互一致性。

## ZCode 固定基线

原生实现必须先满足 ZCode 基线，再使用 Ely 组件承载交互。默认 Ely palette、默认暖白
背景/深棕表面、近似间距或仅凭“看起来相似”的截图都不能作为达标依据。当前固定几何
值为：侧栏 `264px`、桌面外沿 `4px`、标题栏 `48px`、空草稿 Composer 最大宽度
`672px`。普通会话正文和非空 Composer 跟随 ZCode 的会话列断点：可用宽度小于
`864px` 时使用全部可用宽度，`>=864px` 时为
`min(available_width - 96px, 896px)`，`>=1280px` 时为
`min(available_width - 384px, 1152px)`。设置页内容上限仍为 `896px`。这些是 GPUI
逻辑像素，定义在 `style.rs` 的 `SIDEBAR_WIDTH`、`DESKTOP_INSET`、
`TITLEBAR_HEIGHT`、`CONTENT_MAX_WIDTH`、`COMPOSER_MAX_WIDTH` 和
`EMPTY_COMPOSER_MAX_WIDTH`，会话断点由 `conversation_content_width` 统一实现。
Windows 会话内容还保留 `CONVERSATION_SCROLL_GUTTER=15px` 的逻辑像素滚动槽，
消息与底部 dock 共用这段留白。
`TITLEBAR_HEIGHT=48px` 来自固定来源 `WorkspaceHeader.tsx` 的 `h-12`；原生主工作台必须
保留这段布局占位，浮动窗口控制层不能替代它。

Light/Dark 的关键 Palette 角色也由 `style.rs` 固定：`bg` 为 `#f8f8f8` / `#161616`，
`surface` 是 Light `#0d0d0d @ 3%`、Dark `#ffffff @ 5%` 的叠层；`sunken` 与 `overlay`
仍为 `#ffffff` / `#2b2b2b`，输入使用 `sunken`。`fg` 为 `#262626` / `#d4d4d4`，
`accent` 为 `#000000` / `#ffffff`，`link` 为 `#0b7fff` / `#4099ff`。
`ShellColors` 仍分别维护 `canvas`、`sidebar`、`content`、`header`、`panel` 和
`input`，不能把所有层级折叠为一个背景。完整角色、alpha 和字号映射见
[`docs/ely-zcode-style.md`](docs/ely-zcode-style.md)。

## 产品气质

界面面向长时间工作的个人开发者，应该安静、紧凑、清晰并且可快速扫描。侧栏用于在
项目和会话之间定位，主区域用于阅读对话和工具结果，输入区用于连续提交任务，工作台
用于文件、终端、Diff、Git 和工作树操作。

优先保证：

- 主要内容在首屏有稳定的视觉层级；
- 列表、消息、工具输出和错误信息可以快速比较；
- 键盘焦点、快捷键、禁用状态和等待状态始终可理解；
- 长文本、窄窗口、深色主题和高 DPI 下不发生遮挡或跳动；
- 主题、密度和字号改变后，布局仍然使用同一套语义令牌。

避免营销式空白、装饰性渐变、重复套叠的浮层、无意义的边框以及用颜色代替状态说明。

## 主题与颜色

通过 `ActiveTheme` 读取全局 `Theme`，通过 `theme.colors` 选择语义色。业务代码不应
直接写固定颜色来绕过 ZCode palette。常用角色如下：

| 角色 | Ely 调色板字段 | 用途 |
| --- | --- | --- |
| 页面背景 | `bg` | 窗口和主工作区的基础背景 |
| 内容表面 | `surface` | 普通内容区、设置区和面板 |
| 下沉表面 | `sunken` | 输入区、代码和终端等低层级内容 |
| 普通文字 | `fg` | 标题、正文和主要操作 |
| 次要文字 | `fg_muted` | 描述、元数据和弱提示 |
| 边界 | `border` | 分隔线、控件边界和面板边界 |
| 强调 | `accent` | 当前选中、可执行的主要强调 |
| 成功 | `success` | 已完成或可继续的状态 |
| 警告 | `warning` | 等待、运行和需要注意的状态 |
| 危险 | `danger` | 失败、拒绝和破坏性操作 |

语义色只表达真实状态。成功色不能用来装饰普通文本，危险色不能用来制造视觉噪声。
选中、悬停和禁用状态应同时调整表面、文字或边界，使信息不依赖单一颜色通道。

## 字体与密度

- 界面文字统一使用 `ui_text_size(theme, UiTextSize::...)`；`Xs`、`Sm`、`Caption`、
  `Base`、`Lg`、`Xl` 在默认 `font_scale = 1.0` 下分别为 `10px`、`12px`、`13px`、
  `14px`、`16px`、`18px`。来源角色对应 `text-ui-xs` 到 `text-ui-xl`，不另建字号系统。
- 代码、路径、命令、哈希、模型名和终端输出使用 `theme.mono_family`；外围标题和控件
  仍使用界面字体。
- 间距、控件高度和圆角通过 `Density`、`ControlSize`、`Radius` 等主题令牌选择，不能
  为单个页面创建第二套缩放系数。
- 文本可能来自用户或模型，容器必须允许换行或滚动；按钮和菜单项不得因长文案撑坏
  邻接布局。

## 布局

主工作台保持三层结构：

1. 侧栏显示项目、分组、会话和未读状态，宽度变化不应改变主内容的语义顺序。
2. 中央内容区显示当前会话或设置页，标题、状态、历史和实时内容按时间与事实状态排列。
3. 输入区位于中央内容底部，模型、权限、计划、队列和附件等选项与当前会话绑定。

工作台页面可以占用中央区域或作为可关闭面板打开，但不能把文件、终端、Diff 和设置
状态伪装成聊天消息。小窗口使用滚动和折叠，不能通过删掉状态信息换取表面上的完整显示。

## 控件与交互

- 按钮、图标按钮、输入框、选择器、菜单、对话框和设置行优先使用 Ely 组件。
- 只有存在明确动作时才显示主操作。破坏性动作使用 `danger` 和确认步骤；等待中的
  任务禁用会改变同一事实的重复提交。
- 所有可点击控件必须有可见的 hover、focus、pressed、disabled 和 error 反馈；键盘焦点
  不能只通过微弱颜色变化表达。
- 会话、分组、草稿和工作树的操作应使用宿主返回的稳定 ID。UI 不能依据列表位置生成
  持久标识，也不能把标题当作唯一键。
- 流式文本使用有界投影；发生丢帧、重置或版本不一致时重新读取宿主事实，不在界面中
  拼接一份未经确认的替代记录。
- 错误消息应说明可执行的下一步，隐藏凭据、完整请求体和本机敏感路径。

## 状态与持久化

Rust `NativeHost` 和各领域服务是唯一事实源。GPUI 实体可以保存输入草稿、当前焦点、
滚动位置和短期动画状态，但不能宣布任务完成、取消或提交成功。

- Journal 记录会话、消息、工具调用、工作流和终态事实；投影只负责高效显示。
- 快照恢复后必须重新验证项目根、会话 ID、权限和当前 revision。
- 草稿保存使用 `NativeDraftStore` 的有界文件和原子写入；侧栏分组使用 revision CAS
  和持久 operation ID 去重。
- 冷恢复必须重新读取磁盘，不能把当前进程的缓存作为恢复事实。

## 产品边界

目标界面包括本地项目、会话、模型设置、文件编辑、终端、Diff、Git、工作树、插件、
Skills、MCP、Memory、Goal、Automation 和 Rust 工作流。官方账号、支付、云端消息、
机器人、SSH、WSL、Docker 和计算机控制不属于目标产品；不要为这些范围增加设置入口、
菜单项、资源或主题角色。

## 当前验证边界

`out/ely-workspace-tests-12.log` 中记录的测试组均为 `test result: ok`；
`out/ely-workspace-clippy-18.log` 和 `out/ely-workspace-clippy-19.log` 均完成检查。
`out/ely-native/smoke-20/native-run/report.json` 则记录输入后的 Enter 流程未产生
`turn_started`，最终因 Journal 等待超时失败；配套 `screenshots/0009-typed.bmp`
显示输入前缀丢字。以上只构成离线检查和失败诊断证据，产品功能、像素一致性和性能验收
仍保持 `pending`。

上述 `smoke-20` 是历史失败报告，保留用于说明输入丢失诊断。当前 release25 原生报告绑定
binary SHA-256 `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452`：
`full-25` 的工具、会话重开和两轮 Turn 链通过，`settings-dark-25` 与
`settings-light-25` 的八个设置页及最小真实回合通过；`release25-calibration` 与固定
reference6 的差异为 `7.4805878%`，比较状态仍为 `acceptance=pending`、
`assessment=not_identical`。`permission-askuser-25` 的失败来自 UIA helper 默认 DPI 导致
点击坐标偏移，属于 runner 取证问题。上述 release25 证据不代表 release26，也不构成像素、
完整产品或性能验收通过。

## 验收

设计检查应至少覆盖浅色和深色主题、标准和紧凑密度、长文本、错误/等待状态、键盘焦点、
窗口缩放和 Windows 原生窗口。像素比较必须使用同一主题、同一逻辑窗口尺寸、同一 DPI、
同一字体和同一页面状态（包括数据、滚动位置、焦点和队列状态）。证据应包含当前原生
窗口截图或像素 diff，以及对应运行日志或验收报告；历史截图、静态源码映射和不同主题、
DPI、字体的截图都不能证明像素一致。离线 Rust 检查只能证明编译、格式和单元契约；没有
当前 Windows 窗口报告或像素证据的功能保持 `pending`，不能虚报通过。

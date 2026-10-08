# ZCode 固定像素来源与原生实现边界

KeenCode 原生 UI 以 ZCode 3.14.3 的固定提交
`29628c9acdb81b703bbd4080c207a0e7ce5e276e` 作为固定像素验收基线，而不只是设计
参考。它约束桌面工作台的布局密度、聊天和工具信息层级、主题对比度、键盘交互和设置
组织方式；它不规定 Rust 类型、宿主状态或持久化格式。

固定提交已有同平台 Windows 参考证据：`out/ely-native/zcode-windows-reference-5/native-run/report.json`
使用官方 `out/zcode-reference-3.14.3/ZCode.exe`，SHA-256 为
`afb17a861ce435e1ebecb32fa99ca14daa94adadc6054b5f2116b418248cace5`；官方 v3.14.3
annotated tag peeled 到固定提交。该参考使用 Dark、DPI `192`、逻辑窗口 `1280x820`、
物理 client `2560x1640`，最终截图为
`out/ely-native/zcode-windows-reference-5/native-run/screenshots/0017-workspace-final.bmp`，
metrics 为 `metrics/0018-workspace-final.json`。它是 Windows 窗口几何和源页面状态的
同平台参考，不是目标 KeenCode 功能验收报告。

`out/native-live/zcode-source-baseline-29628c9-dpr2/` 中的 PNG 来自 Playwright/Vite 浏览器
路径，只作为内容布局参考；旧浏览器比较的差异比例不能替代同平台参考，也不能单独宣称
像素 `PASS`。

## 来源、许可证与映射边界

ZCode 来源源码的复制范围、文件路径和 SHA-256 记录在
`third-party/zcode/SOURCE-MAPPING.md`，许可证全文在
[`third-party/zcode/LICENSE`](../third-party/zcode/LICENSE)，总归属在
[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)。来源布局、主题和控件材料主要
来自固定提交的 `packages/ui/src/`；当前产品的 GPUI 适配位于
`apps/desktop/src/native_ui/`。这些目录之间是明确的映射边界，不是第二套业务 UI 或
运行时复制清单。

## 来源到实现的映射

| 设计参考 | 当前原生实现 | 说明 |
| --- | --- | --- |
| 工作台整体层级 | `apps/desktop/src/native_ui/main_workbench.rs` | GPUI 根实体组合侧栏、内容区和面板 |
| 项目与会话侧栏 | `apps/desktop/src/native_ui/sidebar.rs` | 使用 Rust 返回的项目、会话和分组事实 |
| 对话与工具结果 | `apps/desktop/src/native_ui/chat.rs` | 只显示 Journal 归约后的有界投影 |
| 输入和提交区 | `apps/desktop/src/native_ui/composer.rs` | 草稿、附件、队列和宿主回执由类型化 API 管理 |
| 设置页 | `apps/desktop/src/native_ui/settings/` | 使用 Ely 设置行和 Rust 设置域 |
| 文件、终端、Diff 和 Git | `apps/desktop/src/native_ui/workbench/` | 作为原生工作台面板提供 |

上表记录的是固定来源到当前原生 GPUI 实现的责任边界。原生组件的颜色、文字、间距、
圆角和控件行为必须使用 `apps/desktop/src/native_ui/style.rs` 定义的 ZCode 令牌和
Ely/GPUI 组件；需要改变参考行为时，以当前 Rust 宿主契约和可执行验收为准。

## 依赖固定

| 依赖 | 来源 | 固定 revision | 许可证 |
| --- | --- | --- | --- |
| GPUI | `https://github.com/zed-industries/zed` | `1a28cff4b409169bac058bca40dfbfeb7621d19b` | Apache-2.0 |
| Ely GPUI Components | `https://github.com/ZacharyZhang-NY/Ely-GPUI-Components` | `94f34c9f8e98b5f4b3078776a4c197b9021f4cdf` | MIT |
| ZCode 设计参考 | ZCode 3.14.3 | `29628c9acdb81b703bbd4080c207a0e7ce5e276e` | Apache-2.0 |

GPUI 通过固定 Cargo git 依赖取得；Ely 使用同一固定 revision 的本地副本
`third-party/ely-gpui-component/`，尺寸与 UIA 小补丁见其 `SOURCE.md`，通过 Cargo
path 依赖编译。ZCode 参考材料的 Apache-2.0 许可证全文保存在
[`third-party/zcode/LICENSE`](../third-party/zcode/LICENSE)。完整归属见
[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)。

## 状态边界

- `NativeHost`、Agent Loop、Provider、Journal、资源和工作流服务保存权威事实。
- GPUI 实体保存当前投影、输入草稿、焦点和滚动等短生命周期状态。
- Journal 顺序、终态、权限、分组 revision 和 operation ID 必须由 Rust 返回；界面不能
  根据按钮点击或列表位置猜测成功结果。
- `NativeDraftStore` 和 `NativeSidebarStore` 使用有界 DTO、原子写入、跨进程锁和冷恢复
  读取，具体持久化约束由对应 Rust 模块和测试定义。
- `WorkflowDefinitionV1` 由 `core/workflow/` 的 Rust `serde` 类型校验；草稿展示结构不
  能改变正在执行的 revision。

## 产品范围

保留本地项目、会话、模型设置、文件编辑、终端、Diff、Git、工作树、插件、Skills、MCP、
Memory、Goal、Automation 和工作流。官方账号、支付、云端消息、机器人、SSH、WSL、
Docker 和计算机控制不属于目标范围，不能从参考材料推导出新入口。

## 固定比较规则

像素比较使用原生实现的逻辑像素和 `style.rs` 的固定值：侧栏 `264px`、桌面外沿 `4px`、
标题栏 `48px`、空草稿 Composer 最大宽度 `672px`，以及普通会话的三段宽度规则：
`<864px` 使用可用宽度，`>=864px` 使用 `min(available_width - 96px, 896px)`，
`>=1280px` 使用 `min(available_width - 384px, 1152px)`。原生
`style.rs::conversation_content_width` 应与这组源规则一致；设置页内容上限仍为
`896px`。Windows 会话内容还保留 `CONVERSATION_SCROLL_GUTTER=15px` 的逻辑像素滚动槽，
消息与底部 dock 共用这段留白。Light/Dark 必须分别使用 `style.rs` 安装的 ZCode palette：
`surface` 为 Light `#0d0d0d @ 3%`、Dark `#ffffff @ 5%` 的叠层，`sunken`、`overlay`
及输入仍为 `#ffffff` / `#2b2b2b`；字号必须沿用同一 `Theme.font_scale` 和 `UiTextSize`
映射。

标题栏尺寸来自固定来源 `packages/ui/src/WorkspaceHeader.tsx` 的 `h-12`（48px）；
原生主工作台必须保留同样的 48px 顶部布局占位，浮动窗口控制层不能替代该占位。

比较双方必须同时固定：

1. 同一主题（Light 或 Dark）和同一 palette。
2. 同一逻辑窗口尺寸、窗口状态和 DPI 缩放。
3. 同一字体、字体加载状态和字号缩放。
4. 同一页面状态、数据、滚动位置、焦点和队列状态。

历史截图、静态源码映射、浏览器预览，以及主题、DPI 或字体不同的截图，都不能证明
像素一致。缺少当前原生窗口截图、像素 diff 和对应运行日志/验收报告时，相关项目保持
`pending`，不得将来源提交存在或静态映射写成验收通过。

当前证据中，`out/ely-workspace-tests-12.log` 的测试组均为 `test result: ok`，
`out/ely-workspace-clippy-18.log` 与 `out/ely-workspace-clippy-19.log` 均完成检查。
`out/ely-native/smoke-20/native-run/report.json` 记录 Enter 后未产生 `turn_started`，
并因 Journal 等待超时失败；配套 `screenshots/0009-typed.bmp` 显示输入前缀丢字。
这些证据不改变产品、像素和性能项目的 `pending` 状态。

## 验收口径

离线 Cargo 检查用于验证格式、编译、测试、Clippy 和依赖审计。Windows 原生窗口验收使用
`tooling/native-gpui-tests/` 的显式计划、隔离数据根和报告。没有当前报告的交互范围保持
`pending`；参考提交、静态映射或历史截图都不能单独证明功能完成，像素验收还必须满足
上面的同主题、同窗口、同 DPI、同字体和同页面状态条件。

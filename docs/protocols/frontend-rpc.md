# Native GPUI Host 契约

本文记录当前 KeenCode 桌面产品的 GPUI 与 Rust Host 边界。现行生产链只使用
`NativeHostApi`、`NativeUiAction`、`NativeActionReceipt` 和 `NativeEventBatch` 等
Rust typed 类型；Rust Host、Journal、Runtime 和本地文件服务是事实权威，GPUI 只保存
可丢弃的当前投影与编辑草稿。

实现锚点是 `apps/desktop/src/native_ui/model.rs`、
`apps/desktop/src/native_host.rs`、`apps/desktop/src/native_host/actions.rs`、
`apps/desktop/src/native_host/subscription.rs` 和
`apps/desktop/src/native_host/launch.rs`、
`apps/desktop/src/diagnostics/process_resources.rs`。当前产品没有 Tauri command、WebView、
`ChannelClient` 或 VQL 运行时；旧桥接材料见本文末尾的历史章节。

## NativeHostApi

`NativeHost` 实现 `NativeHostApi`。所有方法返回 `NativeUiTask<T>`，完成值为
`Result<T, NativeUiError>`；错误带稳定 `code`、脱敏 `message` 和 `retryable`，UI
不得用本地状态把失败伪装成成功。

| 方法 | typed 结果 | 作用 |
| --- | --- | --- |
| `load_workspace(root_path, cursor, include_archived)` | `WorkspacePage` | 读取项目、会话、分组和分页游标 |
| `model_catalog()` | `Vec<ModelCatalog>` | 读取 Provider/model 的无凭据展示目录 |
| `load_conversation(session_id, cursor)` | `ConversationFact` | 从 Journal 投影读取会话正文、队列和草稿 |
| `dispatch(NativeUiAction)` | `NativeActionReceipt` | 执行项目、会话、发送、队列和交互动作 |
| `subscribe_session(session_id, sink)` | `NativeUiSubscription` | 订阅同一 Session 的有界 typed 事件批次 |

动作定义在 `NativeUiAction`。需要持久化或执行确认的变更携带调用方生成的
`operation_id`，包括项目/会话创建、改名、归档、置顶、分组、发送、停止、恢复、回退、
分支、反馈、工具审批、问答和队列修改；只读加载、草稿读取和历史分页不伪造操作回执。

## Receipt 与投影

`NativeActionReceipt` 只有三个字段：`operation_id`、可选的 `session_id` 和可选的
`accepted_sequence`。它由 `NativeHost::execute_action` 在领域操作返回后生成；
`accepted_sequence` 是宿主能够读取到的该 Session 最新 Journal sequence，不代表 UI
可以跳过事件或自行提交事实。UI 应在回执成功后重新读取对应投影，或等待同一连接的事件。

订阅批次为：

```text
NativeEventBatch {
  session_id,
  delivery_sequence,
  journal_sequence,
  events: Vec<NativeUiEvent>,
}
```

`delivery_sequence` 是当前订阅的连续投递序号，`journal_sequence` 是事实序号。批次
可能包含 Snapshot、消息/工具/问答/草稿/队列变化、Turn 状态和 Session 元数据变化。
`ResyncRequired` 表示订阅出现缺口或无法证明连续性；UI 必须丢弃不再可信的增量，重新
调用 `load_conversation`，不能使用 optimistic overlay 补齐缺失事实。`Closed` 只表示
宿主关闭了订阅，UI 应释放本地任务和句柄。

连接和窗口绑定由 `NativeHost` 的 Session lease、订阅表和空闲回收负责。关闭或重开
窗口不会把旧 Session 投影、游标、草稿或事件批次转移到另一连接；GPUI 实体销毁只会
丢弃订阅者，后台 Runtime、PTY 和开发服务仍由 Host 持有并按生命周期显式关闭。

## NativeWorkbench

文件、差异、Git、工作树、编辑器、终端和开发服务不走会话 RPC。启动器在
`native_host/launch.rs` 中将同一份 `NativeWorkbench` 注入 `NativeWorkbenchPanel`；
面板通过 typed Rust service 直接读取/写入事实，并在 blocking 任务完成后刷新快照。
这些 service 返回 Rust `Result`，面板用 loading、status 和 error 投影结果，不建立第二
份前端事实库。工作树和编辑器的具体契约分别见 `git-worktree.md` 与 `local-editors.md`。

面板操作有当前 workspace generation 和串行 loading guard。切换项目或关闭面板后，旧
blocking 结果不能覆盖新根目录；丢弃 GPUI task 也不会假装取消已经启动的文件或 Git
操作。`NativeWorkbenchPanelHandle::close` 只释放 UI 订阅和任务，Host 仍负责外部进程
和 PTY 的收尾。

## Active barrier

一次 turn、停止、恢复、审批和问答的生命周期由 Rust Runtime、Journal 和 typed pending
账本建立与解除。GPUI 可以显示 queued/running/completed 等状态，但不能凭按钮状态
宣布 turn 完成。发送、取消、恢复和关闭必须等待对应 typed task 的回执或明确错误；
连接重建后，旧订阅的事件不得写入新投影。

## 资源诊断

`ProcessResourceSampler` 只采样当前 GPUI Rust 进程，`process_count` 成功时固定为
1；它不枚举、聚合或读取 WebView、JavaScript runtime 或其他子进程。CPU 首次采样
保留未知值，后续按逻辑处理器数量归一；工作集、虚拟内存和平台可用的私有提交量按
`ProcessResourceSample` 字段返回，缺失值保持 `None`，不回退为零或冒充其他指标。

## 验收记录

原生验收器 `tooling/native-gpui-tests` 通过 Win32、PrintWindow 和 AccessKit 与
真实 GPUI 窗口交互，不加载 WebView、CDP、JavaScript 或前端夹具。原生 renderer、
窗口连接、事件重同步、snapshot 恢复和 active barrier 的验收仍以对应计划与
`docs/frontend-acceptance-matrix.md` 中的证据为准；尚未执行的项目必须保持 `pending`。
本文不把静态浏览器或历史 Source contract 结果升级为原生 GPUI 验收。

## 历史：Tauri/VQL 兼容材料（已退役）

以下名称只用于解释旧提交、旧验收报告和来源映射，不是当前可调用接口：

- 旧前端曾通过 `ChannelClient` 使用 VQL 100-204：100 请求、101 取消、102 订阅、
  103 释放，以及 200-204 初始化/成功/错误/序列化错误/事件响应。
- 旧宿主入口曾命名为 `zcode_rpc_open/send/close`，并由 Tauri `Channel` 传递二进制
  载荷；`packages/rpc`、`packages/shared/zcode-protocol-v4` 和旧 Window Controller
  属于该历史链路。
- 旧 Wire/snapshot、`backgroundWorks`、`task_meta_changed`、Source workflow card
  和前端 optimistic store 的描述只适用于当时的 WebView/Source 投影；不能据此新增
  Rust RPC、恢复旧 VQL 编号或在 GPUI 中复制一份状态源。
- 旧资源快照曾把宿主与 WebView2 后代合并统计，并出现 `privateWorkingSetBytes`、
  `privateCommitBytes`、`nativeProcessCount` 等字段；当前纯 GPUI 采样以
  `ProcessResourceSample` 为准，旧字段不能作为现行契约。

保留本节是为了让历史 diff、迁移审查和旧验收日志中的术语可追溯；新功能和修复必须
引用上面的 typed Rust 定义。

### 历史验收记录（状态保留）

下列文字保留旧文档中的验收结论和待办状态，只作为历史证据，不能升级为当前 Native
GPUI 契约：

- 原生 renderer 曾使用源前端的 Shiki WebAssembly 做文件和工具内容高亮；当时的
  `script-src` 只额外开放 `wasm-unsafe-eval`，JavaScript 字符串求值仍未开放，该依赖
  只用于展示、不参与 Rust 工作流执行。
- 当时的产品只承载一个主窗口；验收要求验证非 `main` 请求拒绝、独立连接隔离、取消后
  无悬挂 Promise、重复 close 幂等、seq 缺口触发 resync、snapshot 恢复不覆盖 Journal
  新事实、旧 connection 事件不污染新窗口，以及 wire/snapshot 版本不静默降级。尚未
  执行项目继续在验收矩阵标为 `pending`。
- Assistant feedback 的旧 contract fixture 为
  `tooling/native-live/workflow-contract-fixtures/assistant_feedback_rows.json`，旧
  Source contract 测试位于 `apps/ui/test/frontendContracts.test.ts`；旧原生验收要求
  点击 `v4-feedback-like-{rowId}` / `v4-feedback-dislike-{rowId}`，再从 Journal
  断言 `assistant_feedback_set` 的 `feedback` 值。
- 旧文档记录的 Source45 离线 contract、Native45 workflow controls scope、Native45
  resources precheck 和完整原生验收状态保持原样；这些局部证据不能单独宣称全量 Source
  workflow run pane 或 `backgroundWorks` UI 已通过。

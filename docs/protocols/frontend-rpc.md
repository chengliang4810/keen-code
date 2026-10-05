# 前端 RPC 契约

本文记录 KeenCode 前端与 Rust host 之间的边界。Rust 端的类型和测试是
实现权威；本文是前端接线、审查和验收清单，不替代 Rust 定义。

## ChannelClient 语义

工作区文件查询使用 `file.searchWorkspaceFiles({rootPath, query, limit?, refresh?,
requireQuery?, matchMode?})`。`matchMode` 默认 `fuzzy`，命令中心用
`substring-words` 保留多词、仅文件匹配；最多返回 1000 个原 `WorkspaceFileEntry`。
Rust 先校验已注册的规范工作区，再在 blocking 任务中建立索引和有界堆排序。
最多缓存两个根，每根估算内容预算 32 MiB / 250000 项；超过预算显式报错并提示
`.zcodeignore`，不静默截断。忽略规则支持否定规则，默认排除 Git、依赖和构建目录；
不遍历符号链接或 Windows reparse point。原生事件使索引失效，`refresh` 显式重建，
扫描途中发生变化时保留失效标记。最后一个连接 owner 释放或 LRU 淘汰时停止监听。
旧全量长度/分块接口及前端文件搜索 Worker 已移除，前端只保存当前查询结果；
120ms 输入防抖和卸载/工作区切换的迟到结果丢弃不改变 Rust 的权限与事实权威。

前端继续使用 `packages/rpc` 中的 `ChannelClient` VQL 编号，不为迁移新增
一套消息枚举：

| VQL | 方向 | 语义 |
| ---: | --- | --- |
| 100 | client -> host | Promise 请求 |
| 101 | client -> host | Promise 取消 |
| 102 | client -> host | 订阅事件 |
| 103 | client -> host | 释放事件订阅 |
| 200 | host -> client | 初始化响应 |
| 201 | host -> client | Promise 成功 |
| 202 | host -> client | Promise 错误 |
| 203 | host -> client | 序列化后的 Promise 错误对象 |
| 204 | host -> client | 事件触发 |

`open`、`send`、`close` 只是业务方法名，必须通过上述调用、取消、事件和
响应生命周期实现。请求不可跨窗口复用；`close` 必须释放当前窗口的订阅、
未完成调用和宿主资源，重复关闭应是幂等的。Rust 仅保留有界的最近关闭连接
owner tombstone：同一窗口的迟到 close 成功，其他窗口仍返回 `rpc.windowMismatch`；
tombstone 淘汰后只按未知连接处理，不保留无限关闭连接状态。

Tauri 入口为 `zcode_rpc_open/send/close`。`open` 用原生 `Channel` 接收原始
二进制载荷，`send` 保留原 VQL 序列化，不添加 socket 专用帧头；请求携带
`x-keencode-rpc-connection`，Rust 同时校验窗口所有者。初始化早于 `open`
响应返回时只在有界队列中暂存，取得连接身份后按原顺序交付。

`open` 在返回连接前向本地 Runtime 登记固定的 form Elicitation 能力，不声明 URL
能力。缺少 Runtime 时拒绝建立连接；初始化 Channel 失败时撤销登记，正常 close
及窗口销毁清理同一连接的能力与待决交互。能力登记只描述界面能否展示问答，不能授予
工具权限；工具审批、Plan 守卫与响应所有者校验仍由 Rust 决定。

订阅返回 `202/203` 表示订阅失败，不能当作 `204` 业务事件传给展示回调。
`ChannelClient.onDidEventError` 发出服务、事件和稳定错误码，Tauri 将其写入
有界诊断，不记录请求参数或凭据；失败订阅释放自己的 handler，其他服务调用
继续可用。损坏的二进制载荷和 IPC 传输错误则关闭连接并拒绝所有未完成请求。

实现锚点是 `packages/rpc/src/channels.shared.ts`、
`packages/rpc/src/serialization.ts`、`packages/shared/src/zcode-protocol-v4/`
以及 Rust desktop/frontend RPC 模块。若这些路径在迁移中暂存到其他目录，必须
在来源映射和验收矩阵中记录，不要复制出第二套 VQL 编号。

## Window-bound connection

资源设置页可使用精确的内置 `chat-workspaces/conversation` 根读取插件、命令和 MCP。
它不是已登记项目，不得加入项目列表，也不得因此获得项目级配置写权限。
命令的用户级启停、删除按用户命令根授权；默认对话上下文不会提供项目写根。
conversation 的子目录、兄弟目录及其他未登记路径继续拒绝，路径别名只影响表示。

用户命令自身的 `scope` 为 `global`，但 `location.scope` 为设置目录协议的 `user`；
项目命令两处均为 `project`。`writeCommandFile`、`updateCommandFile` 返回
`{ command: UserCommand }`，不直接返回命令对象。列表筛选、保存回读和启停必须使用这组枚举。

每个原生窗口拥有自己的 connection owner、订阅表和重连状态。主进程可以转发
认证、心跳和帧，但不能把 A 窗口的 host 连接、seq 游标、草稿或恢复结果投影
到 B 窗口。窗口销毁时由该窗口发出释放动作；重开窗口从 Rust Journal 或
snapshot 重新建立投影。

当前产品只创建一个主窗口，项目切换通过 `window_activate_workspace` 激活已登记
的规范目录。跨窗口所有者拒绝由连接级测试验证，不能写成已经验收了两个产品窗口。

## 原生文件通知与资源采样

`file-watcher.watch` 仍只建立所属 connection 的目录描述，`onDynamicChange`
订阅先验证归属并绑定接收器，再启用 Rust `notify` 的原生平台后端。Windows
使用 `ReadDirectoryChangesW`，没有 250ms 递归目录快照或轮询降级。初始注册失败
返回订阅错误；读取事件不触发刷新。最后一个订阅释放时撤销系统监听并等待分发
退出，迟到订阅不能重启已经 `unwatch` 的描述；连接关闭释放其全部资源。

事件仍为 `{ id, dirPath, changedPath }`。单一可信路径保留 `changedPath`；多路径、
重命名组合、后端错误及溢出使用 `changedPath: null`，要求调用方重新读取该目录。
分发只保留一个路径，静默 150ms 后发送，连续变化最多等待 750ms；无变化时阻塞
等待，没有定时目录扫描。事件路径必须位于已授权根内，不能沿符号链接越界。

`desktop_resource_usage_snapshot` 的 `cpuPercent` 使用宿主及 WebView2 后代的
CPU 时间增量，除以采样间隔和整机逻辑处理器数，与共享契约一致。首个样本无基线
时保留未知，不以单核占比冒充整机占比。系统内存只按可见面板的请求刷新，复用
一个 `System`，不执行 `new_all`/`refresh_all` 或采集无关进程。内存继续报告受控
进程 RSS 求和；共享页可能重复计入，不能等同 V8 堆大小或去重后的物理内存。

## Wire 与 snapshot

V4 首次 `sendText(startNow)` 成功后异步安排会话命名，订阅旧根 conversation 时
按同一入口补生成。ACK 只确认根 Turn 开始，后台任务先订阅再读快照；若输入尚未
写入 Journal，则等待权威消息提交，不读取前端草稿或依赖窗口订阅的寿命。
主题固定为首条非 meta、非子 Agent 的用户文本（最多 4000 字符），operationId 由
消息 ID 的 SHA256 导出。仅未命名默认标题或 message-prefix 来源符合自动命名条件。
标题复用会话已选 Provider 的隔离请求、60 秒超时和持久单飞缓存，不携带工具，也不
插入聊天正文；失败不阻塞主回复，下次打开允许重试。生成结果先写 `TitleGenerated`，
再在 Runtime 控制锁内核对原标题及来源，提交 `SessionRenamed(source=automatic)`。
生成期间的手工改名和迟到自动结果不能覆盖当前标题；冷恢复、重复打开不重复生成。

- v4 wire protocol version 固定为 `3`。
- projection snapshot version 固定为 `1`。
- 每个逻辑 frame 带 `logicalFrameId` 和递增 `ordinal`；前端只接受属于
  当前 connection 的连续 `seq`。
- 发现 seq 缺口、重复或 connection 重置时，前端停止把后续事件当作已确认
  状态，发送 resync 请求并等待 snapshot/补帧；不能用本地 optimistic overlay
  冒充已持久化事实。
- snapshot 只恢复显示投影和游标，Journal 仍是会话、消息、工具和工作流的
  权威来源。snapshot 过期或校验失败时丢弃并从 Journal 重建。

任务列表的本地 `workspacePath`/`workspaceKey` 使用前端路径表示：Windows
extended-length 前缀（`\\\\?\\C:\\...` 或 `\\\\?\\UNC\\...`）在返回
Controller、task meta、grouped members/order 和 runtime lifecycle 事件前去除，
并统一为斜杠。Rust 授权、Journal、未读与分组持久化仍使用 canonical 路径；这只是
wire 投影约定，不能在前端另建 workspace 或任务状态。

tasks-index 的行存在性也由 Journal 事实决定：只有 Session 已产生 Turn、Transcript、
可冷恢复的输入队列项或其持久派生事实时才返回任务行。仅有 `SessionCreated` 的
workspace 预热 draft 只可作为 sessions-index detail，不得被 Controller 或旧 task
list 投影为侧栏任务；模型选择、标题、pin/archive 与 create receipt 不构成 promotion。
首次 `send_text` 或 workflow promotion 事实提交后，Rust 只发布一次
`reason: "task_created"`，通知 Source 重新读取同一 tasks-index membership；重复输入
和空 draft 不得重复或提前发布该事件。冷恢复直接从 Journal/task-groups 投影成员，
不依赖进程内事件去重状态。

任务未读 mutation 由 Rust 的 `task-groups.json` 唯一持久事实源负责。旧
`zcode-task.setTaskUnread` 与 Window Controller 的 `mark-read`/`mark-unread` 共用
同一 `expectedUnreadAt` compare-and-clear 规则：只有完成持久化提交并释放存储锁后，
才向 Controller task snapshot 和授权 workspace 的 `onDynamicWorkspaceEvent` 投递
`reason: "task_meta_changed"`。事件中的 `taskMeta.unreadAt` 来自该持久值；未读
mutation 不附带 `unreadSignal`，后者仅保留给真实根 Turn 终态的后台通知。前端不得
为未读状态建立 optimistic 或第二份 store。

conversation snapshot 的 `goal` 由同一 Session 的 `PersistentAgentState` 读取
`GoalFileStore`，不在前端维护副本。Goal 只在 owner Session 的父视图投影；无 Goal、
跨 Session owner 或 virtual child view 不得泄露目标内容。Rust 当前事实源只保存目标
标识、标题、objective、生命周期和累计用时，因此 Source 字段映射为：`active` /
`paused`、完成为 `verified`、阻塞为 `failed`；`iteration` 固定为 `1`，
`activeRunStartedAtMs` 为 `null`，`verifications` 和 `iterations` 为空数组，不能
猜测不存在的验证历史。GoalFileStore 读取或 owner 校验失败必须返回错误，不能伪装为
`goal: null`。

父视图只有 active Goal 才允许 `pauseGoal`，只有 paused Goal 才允许 `resumeGoal`；
无目标和终态均返回带 `reasonCode` 的不可用状态，virtual child view 两者均以
`agent.readOnly` 禁用。pause/resume 的 CAS 提交完成后，Session Gateway 重新投影同一
连接的 snapshot；sendGoalCommand 也走该刷新路径，后续目标生命周期仍以 Journal/Goal
事实驱动的订阅快照为准。Source strict fixture 与隔离/生命周期测试覆盖上述状态，
前端不能通过 optimistic overlay 显示目标。

## Workflow backgroundWorks 投影

`conversation snapshot` 的 `backgroundWorks` 必须从父会话同一份 Workflow Journal 与
`workflowRuns` 投影派生。它不是前端或 Controller 单独维护的事实源，也不能从后台 Agent
列表、旧的本地运行数组或一次性的 UI 事件推断。

Rust 只把属于当前父会话、存在合法 `run-started` 事实且状态仍为 Source 支持的
`pending` 或 `running` 的工作流运行映射到 `backgroundWorks`。终态运行、跨父会话运行、
缺少 `run-started` 的孤立事件、未知或非法的运行状态必须过滤；同一运行的 Journal
更新和 `workflowRuns` projection 使用稳定 `runId` 合并，不能产生重复后台项。恢复、
取消和关闭通过同一 Journal/运行投影收敛，不能另建 `backgroundWorks` 完成事实。

`workflowRuns.runs[].toolCallId` 是 WorkflowHost `WorkflowRunSummary` 的 camelCase
序列化字段，也是聊天工具卡与详情面板之间的唯一关联键。Rust 会先序列化宿主摘要，
再由会话投影保留该字段；不能读取或发送内部 `tool_call_id`，否则后台摘要仍可显示而
详情面板无法打开。

该字段的共享 schema、workflow contract fixture、Controller 契约测试和 Rust projection
均已覆盖；Source UI 通过 `workflowRunCardJoin`、`workflowRunDirectoryModel` 与
`ConversationRowView` 按该值连接工具卡、详情面板和工作流图，缺失字段的历史运行会被
明确过滤，不能伪造详情入口。Source45 离线 contract 与 Native45 workflow controls
scope 已验证这条接线；Native45 resources precheck 和完整原生验收仍待完成，因此这些
局部证据不能单独宣称全量 Source workflow run pane 或 `backgroundWorks` UI 已通过。

工作流运行的 mutation 入口只有 `sendConversationCommandV4` 的 canonical command：
`startSavedWorkflow`、`cancelBackgroundWork`、`resumeWorkflowRun` 和
`amendWorkflowRunSettings`。这些命令统一经过 workspace/Session 归属、busy gate 与持久
command receipt；`zcode-agent` 上的 `start`、`run`、`cancel`、`resume` 及其旧别名、
`resolveQuestion` 直调均不属于公开 RPC。WorkflowHost 的内部 `call` 分支仍只由已授权
Session command 和可信 AgentTool port 使用，不能据此恢复页面直调旁路。

## Assistant 反馈

`setAssistantFeedback` 是会话级 CAS 命令。信封必须携带当前 conversation 的
`baseRevision` 与 `baseLogEpoch`，payload 为 `{ target: { rowId, entityId },
feedback: "like" | "dislike" | null }`。Rust 先按同一份 `rows_for_transcript`
权威投影校验 `rowId/entityId`，并只接受 `assistantText` 行；virtual agent view
是只读入口，不能代发该命令。

接受后 Rust Journal 写入 `assistant_feedback_set`，`SessionState` 只保存按
`rowId/entityId` 定位的本地展示元数据，反馈不会进入 Transcript、Provider 请求或
模型上下文。相同 `commandId`/operationId 重试复用既有事实；Transcript revision
或 log epoch 已变化时返回 `fault.conversation.staleRevision`，不会覆盖新投影。

snapshot 的 Assistant 行使用 Source `assistantTextRowSchema` 的可选枚举字段：
反馈存在时只能是 `"like"` 或 `"dislike"`。取消反馈会删除 Journal 归约状态中的
记录，重新投影时省略 `feedback` 字段，不能发送 `feedback: null`。Source fixture
和契约测试位于 `tooling/native-live/workflow-contract-fixtures/assistant_feedback_rows.json`
与 `apps/ui/test/frontendContracts.test.ts`；原生验收还需通过真实 hover 后点击
`v4-feedback-like-{rowId}` / `v4-feedback-dislike-{rowId}`，并从 Journal 断言
`assistant_feedback_set` 的 `feedback` 值。

## Active barrier

一次 turn 的 `active barrier` 在 Rust host 侧建立和解除。前端可以展示
queued/running/completed 等状态，但不能仅凭界面按钮状态宣布 turn 已结束。
发送、取消、恢复和关闭都要等待对应 RPC 响应或明确错误；连接重建期间，旧
barrier 的事件不得写入新 connection 的投影。

## 验收要点

原生 renderer 使用源前端的 Shiki WebAssembly 做文件和工具内容高亮。
`script-src` 仅额外开放 `wasm-unsafe-eval`，使本地打包的 WASM 可以实例化；
JavaScript 字符串求值仍不开放。该依赖只用于展示，不参与 Rust 工作流执行。

当前产品只承载一个主窗口；窗口归属必须验证非 `main` 请求拒绝和独立连接隔离，
不为了测试增加第二个产品窗口。另需验证取消后无悬挂 Promise、重复 close 幂等、seq 缺口
触发 resync、snapshot 恢复不覆盖 Journal 新事实、旧 connection 事件不会污染
新窗口，以及 wire/snapshot 版本不被静默降级。尚未执行的项目必须在验收矩阵
标为 `pending`。

## 调试与子 Agent 只读目录

`readSessionDebug` 在后端授权 Session 与规范工作区后，读取 Journal 的模型 Round
及实际本地请求记录。Round 最多 200 项，网络记录最多 100 项；未知 Usage、缓存或
生成耗时保留未知。TPS 仅使用 Adapter 记录的首输出到结束耗时，不用请求总耗时替代。
不采集请求/响应 Header，也不把私有接口地址或凭据传入 renderer。

`listSessionSubagents` 从父 Journal 的单层子 Agent 状态和 `actor-started` 工作流
绑定构建目录。普通子 Agent 的展示身份为 `agent:<parentSessionId>:<agentId>`，只读
视图从父 Transcript 按 Agent 过滤；这个身份不能创建新 Journal 或执行会话命令。
调试 Round、网络请求和缓存汇总也按同一 Agent 身份过滤，不包含父或兄弟 Agent 的记录。
工作流 actor 使用已经绑定的真实 Session，并校验父会话、节点地址和规范目录。
完成项按终态时间和稳定身份排序，游标带父 Journal revision；状态变化后旧游标被拒绝，
界面重新读取首页，不跨修订拼接不可确认的页。

父会话的 conversation snapshot 同时携带 `subagents` 运行投影：`revision` 使用父
Journal 的最新 sequence，`childSessionIds` 列出所有合法的一层 virtual view，
`running` 只包含 pending/waiting/running 状态，`endedTotal` 统计已结束的 Agent。
virtual child view 的该字段固定为空，避免在只读子视图中递归显示子 Agent；所有字段均
来自同一份 `SessionState.sub_agents`，前端不得建立第二份状态缓存。

# 2026-09-05 阶段 2.1 后端—前端能力覆盖审计

## 结论

本报告以 `bc8d567` 为原始审计快照，并在阶段 2.1 改动完成后重新检查 Tauri command、前端 IPC 适配器、生产调用点以及 ACP/Peri 事件从生产到界面投影的完整链路。后端 command 面保持对齐：`src-tauri/src` 有 120 个 `#[tauri::command]`，`generate_handler!` 注册 120 项，前端有 120 个类型化 command wrapper（`src/lib/api.ts` 96 个，`src/lib/acp/api.ts` 24 个）。

阶段 2.1 已补齐此前 4 个 API-only command 的生产入口，并闭环两条影响用户状态的事件链：`state_snapshot_meta` 现在经过 `PeriCaps` 双重门控，进入 ACP reducer 和 context usage 投影；后端发出的 `acp://closed` 已有前端类型、监听、活动回合清理和 disconnected 投影。后台任务的 `bg-task-started/completed/cancelled/interacted` 也由摘要面板订阅并触发带视图代次保护的快照刷新。

因此阶段 2.1 的判断是“command、用户入口和本阶段事件链已闭环”，但这不等同于 Windows/Tauri 全功能 E2E 已完成；真实桌面交互、安装包和性能预算仍保留在后续验收范围。

## 审计口径与范围

审计固定使用 `bc8d567` 的 Git 树，避免将测试夹具、字符串命中或并行工作树中的未提交修改混入基线。具体口径如下：

1. 在 `src-tauri/src` 搜索 `#[tauri::command]`，排除 `*_blocking` 内部 helper、测试和供应商补丁文本。
2. 对照 `src-tauri/src/lib.rs:344-468` 的 `tauri::generate_handler!` 注册表，确认 command 定义与注册数量一致。
3. 统计 `src/lib/api.ts` 与 `src/lib/acp/api.ts` 的类型化 `invoke` wrapper；`listen` 和 `listenAcp` 是事件适配器，不计入 command wrapper。
4. 对每个 wrapper 的命令名反向搜索生产调用者，排除 wrapper 定义、测试、fixture、注释和注册表本身。wrapper 存在不等于用户入口存在。
5. 对关键事件逐段追踪“Peri producer → ACP event sink/mapper → Tauri 通知 → 前端类型/解析 → reducer/runtime hook → 可见状态”，任一段缺失都记为未闭环。

## 后端 command 与前端 wrapper 覆盖

### 数量核对

| 审计层 | 证据 | 数量 | 判断 |
| --- | --- | ---: | --- |
| 后端 command 定义 | `src-tauri/src` 的 `#[tauri::command]` | 120 | 已找到 |
| Tauri 注册 | `src-tauri/src/lib.rs:344-468` 的 `generate_handler!` | 120 | 与定义数一致 |
| 前端 command wrapper | `src/lib/api.ts`（96）+ `src/lib/acp/api.ts`（24） | 120 | 与注册数一致 |
| 有生产调用点的 wrapper | 排除定义、测试和适配器后的静态反向搜索 | 120 | 阶段 2.1 已全部接入 |

### 按后端模块的定义分布

| 产品域 | 主要文件 | command 数 |
| --- | --- | ---: |
| 应用设置、诊断、启动和 Provider | `src-tauri/src/lib.rs` | 10 |
| 退出与更新 | `src-tauri/src/app_exit.rs`、`app_updates.rs` | 5 |
| Analytics、个性化、记忆和模型元数据 | `analytics.rs`、`personalization.rs`、`memories.rs`、`model_metadata.rs` | 11 |
| ACP Session、后台任务、Goal 和 replay | `session_commands.rs` | 29 |
| MCP、Skills、Agent、插件和 Marketplace | `extensions.rs` | 28 |
| 项目、文件系统和 Git | `workspace.rs` | 32 |
| 终端与 PTY | `terminal.rs` | 5 |
| **合计** |  | **120** |

数量层面没有发现“定义了但忘记注册”或“注册了但没有类型化适配器”的 command。剩余风险在用户路径：command 可能只能从 wrapper 间接到达，或只被测试/基础设施使用，不能据此宣称界面能力可用。

## 原四个 API-only command 的接入结果

以下四项在原始审计快照 `bc8d567` 上只有后端实现、注册项和前端 wrapper，没有生产 UI/业务调用者。阶段 2.1 已将它们接入已有业务表面，并为成功、失败、空态、幂等和视图切换状态补回归测试。

| 后端 command | 后端实现与注册 | 前端 wrapper | 当前判断 | 应接入的业务语义 |
| --- | --- | --- | --- | --- |
| `diagnostics_log_path` | `src-tauri/src/lib.rs:34`；注册于 `:347` | `src/lib/acp/api.ts:85` `diagnosticsLogPath` | 已接入 | `StatusModal` 展示、打开、定位和复制诊断日志路径，并保留加载/失败反馈 |
| `memories_status` | `src-tauri/src/memories.rs:589`；注册于 `src-tauri/src/lib.rs:399` | `src/lib/api.ts:722` `memoriesStatus` | 已接入 | `PersonalizationSettingsPanel` 展示启用、记忆数量、运行状态和根目录操作 |
| `goal_transition` | `src-tauri/src/session_commands.rs:1250`；注册于 `src-tauri/src/lib.rs:390` | `src/lib/acp/api.ts:259` `goalTransition` | 已接入 | `ComposerGoalProgress` 提供完成/阻塞入口、原因输入、revision 校验和幂等反馈 |
| `background_tasks_cancel_all` | `src-tauri/src/session_commands.rs:399`；注册于 `src-tauri/src/lib.rs:370` | `src/lib/api.ts:56` `backgroundTasksCancelAll` | 已接入 | `ConversationSummaryPanel` 提供跨会话任务统计、确认弹窗、全部取消和结果刷新 |

`diagnostics_record`、`startup_frontend_ready` 和 `session_get_state` 属于诊断、启动和运行时快照基础设施，不要求各自拥有独立用户按钮；它们不作为 API-only 功能缺口。反过来，测试中调用 command、只出现命令字符串、或仅出现在 `generate_handler!` 中，都不能替代生产调用证据。

## 关键事件链缺口

### `state_snapshot_meta`：生产、门控、解析和 UI 投影已闭环

当前链路如下：

```text
peri-agent Act
  → peri-acp event_sink 的 AcpEvent.StateSnapshotMeta
  → peri/agent_event
  → src/lib/acp/events.ts 类型与 parser
  → src/lib/acp/store.ts reducer
  → src/hooks/acp-runtime/events.ts context usage 投影
  → ContextUsageChip 使用持久化的会话级快照
```

证据：

- Peri 在每次 Act 阶段读取 transcript/token tracker 并发出快照元数据：`vendor/peri/peri-agent/src/agent/stages/act.rs:40-67`。
- ACP event sink 已把 `ExecutorEvent::StateSnapshotMeta` 映射为 `AcpEvent::StateSnapshotMeta`：`vendor/peri/peri-acp/src/session/event_sink.rs:316-336`。
- 前端类型和 parser 已识别 `state_snapshot_meta`：`src/lib/acp/events.ts:206-214,418-433`。
- `src/lib/acp/store.ts` 的 `reduceAgentEvent` 已保存完整 `state_snapshot_meta` 字段；`src/hooks/acp-runtime/events.ts` 使用 `deriveStateSnapshotMetaContextUsage`，仅在没有更权威的已知 usage 时更新会话级 context usage，并在当前会话同步状态栏。因此生产快照已形成统一的 Session context projection。

此处还有一个必须先固定的单位契约：`vendor/peri/peri-acp-types/src/event.rs:331` 的注释和测试 fixture 按 `budget_pct` 的 `0.0-1.0` 解释，`vendor/peri/peri-agent/src/agent/token.rs:84` 的 `context_usage_percent()` 却返回 `0-100`；前端 `src/lib/contextUsage.ts:13,61` 的展示百分比也按 `0-100`。不能把当前值直接接到 UI。收口时应明确 wire 单位，并以 `total_tokens/context_total_tokens` 与百分比边界测试锁定行为；推荐在协议边界只保留一种单位，由展示层负责换算。

### `PeriCaps.context_usage`：已协商，未单独门控

`vendor/peri/peri-acp-types/src/peri_caps.rs` 已定义、解析并在 initialize response 中回显 `peri.contextUsage`。`vendor/peri/peri-acp/src/session/event_sink.rs` 现在对 `StateSnapshotMeta` 同时检查 `caps.agent_event` 和 `caps.context_usage`，并用定向测试锁定四种组合。因此：

- `agent_event=true, context_usage=false` 时抑制上下文快照；
- `context_usage=true, agent_event=false` 时同样不发送，因为外层自定义事件通道未打开；
- `caps_registry` 缺失时仍使用内部 `all_enabled()` fallback，属于内部未协商路径，不改变已协商 ACP 客户端的能力边界。

阶段 2.1 的验收矩阵已覆盖 `agent_event/context_usage` 的四种组合、已协商和未协商 Session，以及快照事件不会被重复发送或静默丢失。

### `acp://closed`：后端断开通知已有前端清理路径

`src-tauri/src/peri_runtime.rs:1165` 在 ACP transport 断开后先标记运行时 Session 为 disconnected、清理 turn，再发出 `acp://closed`。`src/lib/acp/api.ts` 的 `AcpEventPayloads` 已加入 `ClosedEnvelope`，`src/hooks/acp-runtime/events.ts` 注册一次性监听并清理活动回合、延迟完成关联、AskUser 和实时投影状态。

异常断开后的前端 loading、active request、pending ask-user 和 Session 视图现在有明确收口路径；已持久化消息和历史内容保留，重复 close 由清空/幂等 reducer 处理。测试覆盖重复 close、正常 disconnect 与传输异常边界。

## 阶段边界

### 阶段 2.1：本阶段已闭环的范围

- 四个 API-only command 接入现有设置、诊断、Goal 和后台任务表面，或在确认无产品语义后删除整条无用面；不能长期只保留“可调用但不可见”的 wrapper。
- 完成 `state_snapshot_meta` 的 `PeriCaps` 门控、协议单位、前端 reducer 和 context usage 投影。
- 完成 `acp://closed` 的前端类型、监听、状态清理和回归测试。
- 在最终 HEAD 重新执行 command 定义/注册/wrapper/生产调用点扫描，再进行真实 Tauri/WebView 路径验证。

### 阶段 2.2：明确推迟

| 能力 | 当前事实 | 推迟原因 |
| --- | --- | --- |
| 标准 ACP Diff | `vendor/peri/peri-acp/src/event/mapper.rs` 当前把工具结果放入文本；前端已有结构化 `diff` parser/render，但后端生产到渲染未贯通 | 需要确定工具输出 schema、旧文本兼容和标准 ACP `ToolCallUpdate` 内容边界 |
| `compact_error`、`context_warning`、`background_task_completed` | 前端能解析部分事件，但 event sink/mapper 没有完整生产投递，reducer 也没有完整 UI projection | 需要逐事件补齐生产者、路由、关联 request/session 和可见状态 |
| `session/execute-command` | `vendor/peri/peri-acp/src/dispatch/execute_command.rs:41-140` 及测试存在；Host `handle_request` 没有路由分支，当前斜杠命令仍由 `session/prompt` 内部拦截 | 需要补 ACP method 路由、取消/完成信号和外部客户端契约 |

### 阶段 2.3：已有部分 UI，但契约仍不完整

- Hook：已有 unsupported hook 元数据显示，`FileChanged` 等真实生产触发点仍缺失；对应 `PRE-004`。
- LSP：已有数量、重启提示和配置装配，但完整诊断、进程生命周期和错误状态 UI 尚未贯通。
- Marketplace 元数据：已有版本、LSP 数量等摘要，完整作者、来源、能力和安装状态契约尚未统一。

这些能力不应通过先增加一个 UI 开关来假装完成；必须等生产触发、协议字段和错误生命周期一起闭环。

## 本阶段不计为缺口的内部项

- `fs_list_dir_blocking`、`git_*_blocking`、`plugin_install_blocking` 等是 command 内部调用的同步 helper，不是额外 command，也不应重复计数。
- `src/lib/api.ts`、`src/lib/acp/api.ts` 的 typed wrapper 是 IPC 适配层，不是独立功能；其数量只能用于检查适配完整性。
- `diagnostics_record`、`startup_frontend_ready`、`session_get_state` 是基础设施调用，不要求独立按钮或设置页入口。
- 测试夹具、字符串扫描命中、注释、注册表条目和仅用于构建的调用，不作为生产覆盖证据。

## 相关预存问题与验证边界

下列问题来自 `docs/maintenance/2026-08-31-preexisting-issues.md`，本报告只引用其影响，不把历史记录直接当作当前绿色证据：

| 问题 | 对本报告的影响 |
| --- | --- |
| `PRE-002` 关键事件通道满时可能丢失 | 事件链补齐后仍需验证关键状态可恢复，不能只测正常流 |
| `PRE-004` FileChanged Hook 没有生产触发点 | Hook 明确留在 2.3，不作为 2.1 command 覆盖完成的证明 |
| `PRE-006` 子 Agent 完成不自动唤醒已结束父回合 | 后台任务完成语义与本报告的 cancel-all command 分开验收 |
| `PRE-010` 构建 chunk/动态导入警告 | 构建成功不代表体积预算达标，也不代表事件/command 覆盖完整 |
| `PRE-012` Goal steering 仍需行为验证 | `goal_transition` 接入后仍要做真实 Goal 行为测试 |
| `PRE-019` Plan 模式原生 E2E 未验证 | 不能用 parser/unit test 代替真实桌面时间线和落盘检查 |
| `PRE-020` 目录选择、拖入、排序持久化等桌面路径未覆盖 | command 有 wrapper 不等于对应 Windows/Tauri 入口已可用 |
| `PRE-021` Windows 窗口控制、PTY 等仍需真实复验 | 本报告不宣称已完成原生 Windows 全功能 E2E |

### 验证结果

| 检查 | 结果 |
| --- | --- |
| HEAD 静态 command 定义/注册/wrapper 审计 | 120 / 120 / 120；120 项有生产调用点，0 项 API-only |
| ACP/上下文及阶段 2.1 全量 Vitest | 123 个文件、1010 项通过 |
| `npm.cmd run typecheck` | 通过 |
| `npm.cmd run lint:css` | 通过 |
| `npm.cmd run build` | 通过；保留既有动态导入和大 chunk 警告（对应 `PRE-010`，不属于本阶段失败） |
| Peri/Tauri Rust 定向检查 | `cargo fmt --check`、`cargo check --workspace --all-targets`、`cargo test --workspace --all-targets` 通过；Peri 与 Tauri 分别完成，无失败 |
| Tauri/WebView/Windows 真实交互 | 尚未完成，不能以静态或代理状态替代 |
| 阶段 2.1 最终验收 | command/事件链代码与自动化验证完成；真实桌面交互仍是剩余风险 |

## 建议的收口顺序

1. 阶段 2.1 已完成四个 API-only command 的生产调用点、事件门控、状态投影和摘要刷新订阅。
2. 已固定 `budget_pct` wire 单位为 `0-100`，并以总 token/上下文总量边界测试锁定展示换算。
3. 已加入 `acp://closed` 的类型、监听和幂等清理回归，覆盖异常断开和正常关闭。
4. 仍需在发布验收阶段补齐真实 Windows/Tauri/WebView 交互、安装包体积和性能基线；这些不应由静态或代理状态冒充。

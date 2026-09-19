# KeenCode 小窗口上下文准入与压缩修复计划

## 背景（codex 会话结论核实）

benchmark 批次 10 题失败的三个真实根因，已逐一在源码中定位：

| 失败类型 | 数量 | 根因定位 |
|---|---|---|
| `context_initial_request_oversized` (7) | 固定输入（system prompt + 工具定义 + 记忆注入）已超有效输入预算，但无独立准入预检，首轮落入压缩臂后因无可压缩历史报 `NothingCompressible`（context.rs:1811-1813 → runner.rs:2101 压缩臂） |
| `context_compaction_request_oversized` (1，17,011 > 16,384) | `ProviderContextCompressor::summarize_call`（context.rs:516-549）的输出钳制只取 `min(provider.max_output_tokens, request.max_output_tokens)`，**没有取窗口一半**，也未经 `largest_fitting_summary_output` 二分收缩；分块路径 `split_source_chunks` 的单原子单元检查（context.rs:2031-2040）同样按未钳制的 `budget.max_output_tokens` 估算 |
| `context_no_safe_history` (2) | micro projection 与摘要共享同一受保护内容定义：`plan_micro_compaction`（context.rs:2425）只投影 stale 窗口外的 Tool 消息，纯受保护历史下无候选 → `NothingCompressible`；机械截断同样跳过 protected（context.rs:1423-1427），三个降级路径全部空转 |

## 修复 1：初始请求准入预检（改 runner.rs + context.rs）

在 `run_active`（runner.rs:1930）进入 Round 循环前、首轮 `PreparingContext` 阶段加一次准入判定（窗口已知时）：

- 新增 `ContextManager::admission_check(request, capabilities)`（context.rs）：
  - 估算固定输入 = 全量逐块 `estimate_request_unanchored`（首轮无锚，天然同口径）；
  - 输入预算 = `input_budget`（窗口 − max(请求输出, 4096) 钳窗口一半）；
  - 固定输入 ≤ 预算 → `Admitted`；
  - 固定输入 > 预算但剔除工具定义后能装入 → `Admitted`（工具目录通知已有 fail-closed 检查兜底，runner.rs:2265-2300）；
  - 固定输入 > 预算且无法通过压缩历史腾出空间（首轮历史只有受保护的 system/developer + 当前 user）→ 返回结构化诊断 `AdmissionBlocked { input_budget, reserved_output, estimated_fixed_input, breakdown: {tools, messages, fixed_overhead} }`。
- Runner 收到 `AdmissionBlocked` 时直接以 `AgentRunError::Context(新变体 ContextError::InitialRequestOversized { breakdown })` 终止，**不再进入压缩臂**。
- 诊断信息通过现有 `ContextCompactionFailed`/TurnStopped 链路落 Journal：`context_compaction_failure_kind`（runner.rs:6024）把新变体归入 `Budget` 类，Display 输出各预算分项（中文、含数字），满足“输出各预算分项”要求。仅首轮（round 1）做该预检，后续轮次沿用既有触发线。

## 修复 2：压缩请求严格分块（改 context.rs）

让摘要输出的钳制与分块全部以真实窗口为准：

1. **`ProviderContextCompressor::summarize_call`（context.rs:516-522）**：输出钳制从两方 min 改为与 `summary_output_ceiling` 同一三层取小——`min(策略值, provider.max_output_tokens, 窗口/2)`；窗口已知时再用 `largest_fitting_summary_output`（context.rs:1950，带空消息的摘要模板本身开销极小）把输出收缩到 `估算输入 + 输出 ≤ 窗口` 内。这样 16K 窗口下任何摘要调用输出的预留都不可能把请求顶到 17,011。
2. **`split_source_chunks` 单原子单元超限（context.rs:2031-2040）**：单原子单元装不下时，先对该单元做一次零 LLM 的强制工具结果截断投影（复用 `project_tool_result_text`，按 `budget.max_input_tokens` 二分 head/tail 长度），投影后再放得下则继续分块；仍放不下才报 `CompressionRequestTooLarge`。
3. **口径统一**：`summarize_call` 的检查已有；补齐 `summary_budget`（context.rs:1533）在 `largest_fitting_summary_output` 返回 `None` 时的诊断字段（复用现有 `CompressionRequestTooLarge`）。

## 修复 3：no_safe_history 可恢复策略（改 context.rs + runner.rs）

在终止前按序增加两个降级层：

1. **扩大 micro projection 候选**（`plan_micro_compaction`，context.rs:2425）：当历史中**不存在任何非保护可压缩单元**（即 `plan_replacement` 会失败的情形）时，解除 stale 窗口限制——把“最近 3 轮内”的 ToolResult 也纳入投影候选（保持 system/developer、工具调用配对边界不变，只改 ToolResult 文本内容）。这直接覆盖 CF4/SP3 这类“首轮大任务材料 + 少量轮次”的形态。
2. **机械截断兜底前先试 micro**：`try_mechanical_truncation_fallback`（runner.rs:1514）入口先跑一次 `plan_micro_compaction`（含第 1 步的放宽模式），有收益即应用投影后再做机械截断/重试判定。
3. **真无可压缩单元时的明确诊断**：`NothingCompressible` Display 从笼统一句改为结构化说明——新增 `ContextError::ProtectedInputFillsWindow { estimated_input, input_budget, protected_units, recent_window_units }` 变体（`plan_replacement` 在“候选存在但全部 protected”时返回它），Display 输出“受保护输入占满窗口”的分项明细。`NothingCompressible` 保留给“确实没有候选单元”的情形。两变体在 `runner.rs:2129-2136` 的 soft_failure 白名单中同列。

## 不做的事

- 不改 `TurnStopReason` 枚举（`context_blocked` 已够用，细分分类靠错误 Display 与 benchmark 侧解析，遵循“不保留旧枚举别名”的项目规则）；
- 不动 `TranscriptUnit` 安全边界（system/developer 保护、工具配对原子性全部保持）；
- 不动 Journal/事件 schema（现有 `ContextCompactionStarted/Failed/Applied` + `turn_stopped(reason=context_blocked)` 足够承载）。

## 测试计划（全部在 context_tests.rs / runner 现有测试模式下）

用 `FixedEstimator` + `bounded_test_context` / `direct_policy` 模式新增：

1. **准入预检**：窗口 7,380 固定输入超限场景（复刻 `runner_uncompressible_precompression_obeys_hard_budget`，context_tests.rs:1542）改为断言：不进入压缩臂、错误为 `InitialRequestOversized` 且 Display 含各预算分项；
2. **摘要输出钳制**：16K 窗口 + provider max_output 32K 场景，断言 `summarize_call` 实际发出的 `max_output_tokens` ≤ 窗口一半，且估算 `输入+输出 ≤ 窗口`（复刻 17,011 失败形态，断言不再复现）；
3. **单原子单元投影**：单条超大工具结果装不进摘要预算 → 断言先被强制投影再成功分块摘要；
4. **放宽 micro**：纯受保护历史（system + 不完整工具交换 + 近 3 轮 ToolResult）→ 断言 micro 投影有候选、压缩成功，不再 `NothingCompressible`；
5. **真满窗诊断**：全部内容均不可触碰 → 断言新变体 `ProtectedInputFillsWindow` 且 Display 含分项。

## 验证

```bash
cargo test --manifest-path crates/keencode-agent/Cargo.toml -p keencode-agent
cargo fmt --manifest-path crates/keencode-agent/Cargo.toml
cargo clippy --manifest-path crates/keencode-agent/Cargo.toml -p keencode-agent
```

前端与资源层不受影响，无需 typecheck。改动集中在 `crates/keencode-agent/src/context.rs`、`runner.rs`、`context_tests.rs` 三个文件。
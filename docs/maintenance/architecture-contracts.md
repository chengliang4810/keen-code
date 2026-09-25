# 跨模块契约（ARC 试点）

本文档记录 KeenCode 中**跨模块不变量**：那些一旦漂移就会静默出错、且无法靠单端
测试发现的约定。每条契约给出定义、违反后果与可执行的验证方式。

试点范围刻意收窄：只收录已经出现过漂移、或本次改动新增且容易在后续迭代中被
破坏的契约。不要为了"完整"扩充本文档——契约的价值来自每条都有人验证。

验证命令均从仓库根目录执行。

---

## ARC-TOOL-CONCURRENCY-001：工具并发判定必须 fail-closed

**不变量**：`AgentTool::concurrency_for(input)` 只在能**静态证明**本次调用不改变
外部状态时才返回 `ParallelReadOnly`；任何无法确认的输入必须返回 `Exclusive`。
`AgentTool::effect` 的返回值不受此影响——Shell 工具的 `effect` 恒为
`ChangesState`（Plan 守卫依赖该契约），并发判定是独立通道。

**违反后果**：写命令与只读命令进入同一并行批次，破坏顺序副作用屏障。例如
`rm -rf build` 与 `cat build/log.txt` 同批并发会读到不一致状态；判错方向不对称，
把只读误判为独占只损失并行度，反之是正确性缺陷。

**验证**：

```sh
cargo test -p keencode-tools --lib read_only
cargo test -p keencode-tools --lib malformed_input_fails_closed
```

`core/tools/src/read_only.rs` 的 `classify_bash_command` 是唯一判定入口；新增
只读命令时必须同时补正例与反例（写形态）测试。

---

## ARC-TOOL-WRITE-FRESHNESS-001：整文件覆写必须有新鲜的内容认知

**不变量**：`Write` 覆写**已存在且可读的文本文件**前，本 Session 必须已通过
`Read` 登记过该文件的指纹（`size` + `mtime`），且登记后文件未被外部改动。
二进制文件（含 NUL 字节）无法经 `Read` 观察，不适用该要求。工具自身完成写入
后必须立即登记新指纹，使连续覆写无需重读。

**违反后果**：模型基于陈旧读取整文件覆写，静默丢弃用户或其他进程的外部改动。
`ensure_file_unchanged` 只覆盖单次工具执行窗口内的并发，不覆盖"跨轮"的陈旧写。

**验证**：

```sh
cargo test -p keencode-tools --lib write_requires_fresh_read_of_existing_text_file
cargo test -p keencode-tools --lib write_new_file_then_overwrite_without_reread
cargo test -p keencode-tools --lib write_binary_file_is_not_blocked_by_read_requirement
```

实现位于 `core/tools/src/environment.rs`（`ensure_write_is_fresh` /
`record_read`）与 `core/tools/src/filesystem.rs`（`record_written_file`）。
新增任何写文件的工具都必须走同一登记路径。

---

## ARC-PROVIDER-RETRY-CLASSIFICATION-001：可重试性由分类器决定，不由调用点决定

**不变量**：`ModelError` 的 `retryable` 属性只在 `core/provider/src/http.rs` 的
`classify_http_error` 中判定；调用点（`is_retryable_failure`）只读取该属性并叠加
策略开关。远端瞬时故障必须分类为可重试，包括 **Anthropic 过载 529**、
Cloudflare 5xx（520-524、527）、HTTP 408 与 500/502/503/504。

**违反后果**：瞬时故障被判为终态，一次过载直接终止整个 Turn。529 曾在状态码
通配分支中落入 `retryable: false`，高峰期表现为"莫名其妙地失败且不重试"。

**验证**：

```sh
cargo test -p keencode-provider --lib anthropic_overload_529_is_retryable
cargo test -p keencode-provider --lib in_band_overloaded_error_is_retryable_without_status
cargo test -p keencode-provider --lib retry_
```

注意 in-band 路径（HTTP 200 正文携带错误）用固定状态码调用分类器，因此关键词
层必须能独立识别 `overloaded`；只加状态码分支不足以覆盖该路径。

---

## ARC-MODEL-STREAM-PARTIAL-001：已流出的正文不得因中断而丢失

**不变量**：模型事件流在协议终态前中断时，`collect_model_stream` 必须通过
`ModelError::stream_partial_text()` 携带中断前已确认的正文与推理文本；Agent
Runner 必须把它提交进 Transcript。**工具调用参数不进入部分产出**——参数可能被
截断，据其执行或续跑都不安全。

**违反后果**：用户已在界面上看到的内容从持久历史中消失，下一轮上下文与用户
所见不一致。

**验证**：

```sh
cargo test -p keencode-model --lib collector_carries_partial_text_on_stream_interruption
cargo test -p keencode-model --lib collector_omits_partial_text_when_nothing_was_emitted
cargo test -p keencode-model --lib collector_excludes_truncated_tool_arguments_from_partial_text
```

实现位于 `core/model/src/stream.rs`（`partial_stream_text`）与
`core/agent/src/runner.rs`（`commit_partial_stream_text`）。脱敏路径
（`core/provider/src/http.rs` 的 `safe_error_message` 分支）必须原样保留该字段。

---

## ARC-CACHE-PREFIX-DIAGNOSTICS-001：缓存命中率骤降必须可观测

**不变量**：相邻两轮之间，当上一轮缓存命中率 ≥ 0.5 且跌幅 ≥ 0.3 时，必须产生
一条诊断日志。命中率缺失（Provider 未报告缓存字段）时不产生任何判定。

**违反后果**：缓存前缀被改动（工具表变化、注入内容重排、压缩替换历史）导致整条
前缀按未命中计价，成本可能等于数十轮正常增量，而这种异常混在正常账单里无法察觉。

**验证**：

```sh
cargo test -p keencode-agent --lib cache_break_detection_reports_only_significant_drops
```

实现位于 `core/agent/src/context.rs`（`cache_break_event`）。该判定只写日志，
不得改变任何请求行为或压缩决策。

---

## 维护规则

- 新增契约必须同时给出：不变量陈述、违反后果、可执行验证命令、实现位置。
- 契约变更（阈值、字段、路径）必须同步更新本文档；验证命令必须保持可执行。
- 只收录"漂移会静默出错"的跨模块约定。模块内部的实现细节写进代码注释。
- 本试点不引入 CI 门禁脚本：验证命令由改动者在提交前手动执行。若后续契约数量
  增长到难以手动维护，再考虑把上述 `cargo test` 命令固化为一个 CI job。

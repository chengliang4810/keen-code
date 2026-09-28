# 正式环境会话日志排查（2026-09-21）

## 范围与方法

- 数据根目录：`~/.keencode`（正式环境），未修改任何会话、快照或日志。
- 检查对象：17 个 `sessions/*/events.jsonl` 及对应 `snapshot.json`。
- 判定依据：快照中的 turn `status/stopReason/outcomeMessage`、事件日志终态，以及当前仓库源码和提交记录。
- 重要限制：这些会话最后更新时间集中在 2026-09-10 至 2026-09-11；本次只能判断当前源码是否包含针对性处理，不能把旧会话没有再次报错当成线上回归验证。

## 异常分类

| 类别 | 会话 | 日志证据 | 当前判断 |
|---|---|---|---|
| 工具 Round 持久化预检失败 / `context_blocked` | `session-29e0...`（8 个失败 turn） | `outcomeMessage=工具 Round 持久化预检失败：工具 Round 已知内容无法无损持久化` | **部分修复，历史记录未修复**。当前代码已经保留 `context_blocked` 终态、上下文压缩事件和停止原因（`apps/desktop/src/agent_runtime/interruption_context.rs`、`core/agent/src/runner.rs`）；但旧会话中仍有 8 次真实失败，且该会话仍标记 `running`，需要重新打开/继续验证。 |
| 模型服务限流 | `session-486957...`、`session-ab7e...` | `[1302] 账户已达到速率限制` | **非产品缺陷，未“修复”**。当前 provider 已有 rate-limit 分类和重试策略，但不能消除供应商账户配额；应降低并发、等待限流窗口或更换额度。 |
| 达到运行轮次上限 64 | `session-4fc685...`（2 个失败 turn） | `stopReason=limit_reached`，`Round 达到运行上限 64` | **未修复/仍可复现风险**。当前运行上限仍是硬终止语义；需要单独确认是否应提高上限或检测不收敛循环，不能把它归为模型服务失败。 |
| 模型服务连接拒绝 | `session-e702...` | `tcp connect error: Connection refused (os error 61)` | **外部环境失败**。当前代码已有 provider 连接错误归类和用户可见错误保留；日志本身不能证明服务端或代理已恢复。 |
| 用户取消 | `session-c144...` | `stopReason=cancelled`，`Agent Turn 已取消` | **按设计行为，不是缺陷**。同一会话另有一个 turn 仍标记 `running`，需检查是否为快照未收尾。 |
| 历史会话仍标记 `running` | `session-01d425...`、`session-0e38...`、`session-29e0...`、`session-482...`、`session-486...`、`session-4fc...`、`session-771...`、`session-91...`、`session-c144...`、`session-e48...`、`session-e7...` | 快照 `state.status=running` 或 turn `status=running`，但更新时间早于本次检查 | **未确认修复**。这更像旧进程退出/恢复流程没有把终态写回，不能仅凭日志判定当前仍在执行。应在现行版本启动后逐个 `session/load`，核对 runtime 状态、Journal 尾部和进程存活。 |

## 没有发现新的异常终态的会话

其余会话的事件日志中没有独立的异常终态；仅出现错误文本或模型输出中的 `error/failed` 不能直接算产品失败，因为事件类型是成功的 `tool_completed` 或正常 `turn_completed`。

## 当前未修复清单

1. **上下文持久化/上下文阻断**：旧会话已有 8 次 `context_blocked`。当前实现具备结构化记录和恢复提示，但没有本次生产重放证据，状态为“代码有处理，线上修复未证实”。
2. **运行轮次上限导致不收敛**：2 个 turn 达到 64 轮后失败，当前仍需产品决策和专项验证。
3. **历史 `running` 状态收尾**：至少 11 个会话/turn 留在 running，需运行时恢复验收；这不是前端截图或浏览器测试可替代的检查。

## 不应作为代码缺陷处理

- 供应商账户限流：应作为容量/并发配置问题处理。
- 模型服务 `Connection refused`：应检查 provider、代理和服务进程；现有日志不足以归因到 KeenCode。
- 用户取消：按设计保留取消终态。

## 后续动作

1. 使用当前构建启动正式桌面进程，确认只由一个进程持有 `~/.keencode`，避免开发环境与正式环境并发写入。
2. 对上述 11 个 running 会话逐个执行加载，采集 `runtime.state`、Journal 尾事件、`session/recovery` 和进程状态，确认是“可恢复运行”还是“僵尸状态”。
3. 新建一个长上下文会话和一个会触发工具循环的会话，验证 `context_compaction_started/completed/failed`、`context_blocked` 和最终 `turn_stopped` 是否成对落盘。
4. 单独复现 64 轮上限场景，决定是增加收敛保护还是调整上限；在决定前不要把该类错误标记为已修复。

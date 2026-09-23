# 2026-09-23 工作流引擎与子代理调度：下一迭代工作分解（设计存档）

> 状态：设计存档，未实施。本文档整理 2026-09-23 目录重组与 ZCode 模式第一阶段
> 落地（通知泉/批处理/model-only、/workflow Journal/Resume/Amend 顺序运行器）
> 之后的剩余差距，供确认优先级后逐项实施。
>
> ZCode 侧参照结构研究已完成并存档（AskScheduler / actor / dynamic-workflow
> 的 file:line 级架构图，见本会话任务记录）；本文只引用结论，不重复展开。

## 工作分解

### W1. 子代理并发 ask 调度与 actor 模型最小重写

- 现状：子代理运行于共享 coordinator 树 + `GlobalTurnLimiter` 全局 FIFO 槽位
  （`core/agent/src/collaboration.rs`）；完成通知/白名单/模型链已有实现与契约
  测试。缺：per-ask journal 记录、hold/replay 规则、显式并发上限调度器抽象。
- 目标（ZCode AskScheduler 对应物）：
  - `AskRecord`：每个 ask（子代理回合请求）的 journal 记录（序号、状态、
    结果摘要、重试计数），先落 `core/agent` 内存 + 既有 Session Journal 事件。
  - `AskScheduler`：并发上限（对齐全局 `GlobalTurnLimiter` 槽位）、FIFO +
    hold 规则（依赖前序 ask 的 ask 挂起而非失败）、terminal 统一经通知泉。
- 验收：并发 ask 单测（上限、hold、完成通知去重）；既有 700 项桌面测试不回退。
- 风险：触及 `collaboration.rs` 状态机；需与 `GlobalTurnLimiter` 的
  `dispatching` RAII 守卫（已落地）协同，避免重入。

### W2. /workflow 动态脚本调度

- 现状：`/workflow {"steps":[...]}` 顺序运行器 + `WorkflowJournal` 持久化 +
  resume/amend 命令（`apps/desktop/src-tauri/src/acp_host.rs`）。
- 目标（最小动态调度，不含 TS 编译器）：
  - 步骤图：`{"groups":[{"steps":["a","b"],"concurrency":2,"depends":["g0"]}]}`
    ——同组步骤并发执行（各自为独立 detached 回合或子代理回合），组间按
    depends 编排。
  - 调度器落在 workflow 运行器内：组内并发受全局槽位约束；Journal 记录组与
    步骤两级状态；amend/resume 语义沿用（组级重跑）。
- 验收：合成多组工作流的集成测试（并发执行、失败中止、resume 跳过已完成组）。
- 风险：同一 Session 的并发回合需要子代理通道（依赖 W1）或保持顺序降级。

### W3. /workflow 的 TS 脚本 typecheck（独立大件，最后做）

- 目标：对齐 ZCode dynamic-workflow 的 facade 脚本（`.ts` 脚本 + typecheck +
  引擎执行）。ZCode 侧为 TS 编译器 + 子进程 NDJSON host bridge，属独立编译器
  工程（facade d.ts、站点图/污点分析、因果图诊断、schema 生成）。
- 建议：W2 的 JSON 步骤图先行；TS 脚本在 W2 调度器上作为"脚本编译为步骤图"
  的前置层接入，避免一次性引入编译器。
- 风险：TS 工具链在宿主进程外的执行边界（子进程 + 超时）需独立设计。

### W4. 通知跨重启 ledger

- 现状：通知去重为进程内存 `HashSet`（通知泉内），重启后同一任务的完成通知
  可能重复注入一次。
- 目标：把 `notified` 水位持久化（app 数据根 `workflows/notifications.json` 或
  复用 WorkflowJournal 目录），启动时加载；写入与删除沿用原子写模式。
- 验收：单测覆盖"重启后不重复通知"。

### W5. A5-2 残余：terminal_close 主线程语义

- 已落地：`TerminalSession.writer` 改为 `Arc<Mutex<…>>`，阻塞写不再持有
  sessions 全局锁（`20bbd6d5`）。
- 残余：`terminal_close` 仍为同步命令（主线程取 map 锁 + 收割子进程）。
- 目标：`terminal_close` 改 async；收割移至 blocking 线程。
- 风险：低；需回归终端测试。

## 实施顺序建议

W4（最小）→ W5 → W1 → W2 → W3。W1 是 W2 并发组的前置；W3 依赖 W2。

## 明确不在本分解范围

- 合并 `GlobalTurnLimiter` 与子代理并发的深层重构（已有 RAII 守卫修复，
  行为验证由既有 140 项 collaboration 测试覆盖）。
- 提示词口径变更（A2-1 增量估算已按「逐消息序列化 JSON + 常量包装」前缀和
  落地，`dd643211`；104 项 context 测试通过）。

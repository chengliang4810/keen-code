# peri-agent

## Scope

`peri-agent` 提供会话、消息、RCRA 执行阶段、LLM 抽象、工具 trait 与中间件接口；不承担 ACP transport 或桌面界面状态。

## 数据流

`MessageQueue → Receive → Compact → Reason → Act → MessageQueue`。`Receive` 排空队列并决定循环是否退出；`Reason` 投影消息和可见工具；`Act` 分发工具并把后续工作送回队列。会话的冻结数据在创建后保持不变。

## 任务路由

| 任务 | 优先读取 |
| --- | --- |
| RCRA、Prompt 冻结、工具 direct/deferred | `src/agent/` 与 `src/session/` |
| Rust、async 与 doc tests | 目标模块及同目录测试 |
| compact 阈值与环境覆盖 | `src/agent/compact_v2/config.rs` 的 `CompactConfig` |

## 稳定不变量

- `run_react_loop` 是阶段循环入口；退出判断保留在 `Receive`。
- `StageContext` 是阶段依赖边界；阶段间通过输入、输出和上下文传递，不绕过为全局状态。
- `FrozenContext` 的 prompt、`AGENTS.md`、skills 与日期在会话内不可漂移；SubAgent 复用上游冻结数据。
- `BaseTool::is_direct()` 是工具可见性事实源；deferred 工具经搜索和执行代理访问。
- Compact 行为、阈值和环境覆盖只引用 `CompactConfig`。

## 验证

```bash
cargo check -p peri-agent
cargo test -p peri-agent --lib
cargo test -p peri-agent --doc
```

- RCRA 或 compact 改动：运行目标测试；无对应自动测试时，人工追踪 `run_react_loop` 到各 stage 的输入、输出和队列回流。
- 冻结上下文或工具可见性改动：同时检查调用方与包装层。

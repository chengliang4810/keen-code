# peri-acp

## Scope

`peri-acp` 负责 ACP 服务层：Session 生命周期、Prompt 构建、Agent 与中间件装配、事件映射与发送；不实现桌面界面。

## 数据流

`ACP request → SessionManager → frozen session data / prompt → agent builder → run_react_loop → ExecutorEvent → event mapper / event sink → SessionUpdate 或扩展通知 → client`。

## 任务路由

| 任务 | 优先读取 |
| --- | --- |
| Session、事件、Prompt 与工具 | `src/session/`、`src/event/`、`src/prompt/` |
| 中间件具体链顺序 | `../peri-middlewares/AGENTS.md` 与 `src/agent/builder.rs` |
| ACP 服务请求与通知 | `src/server/` |
| 测试位置与覆盖 | 目标模块的 `*_test.rs` 和 `tests/` |

## 稳定不变量

- `SessionManager` 在每条 `session/new`、load、resume 或 fork 路径注册 Session caps；发送扩展事件前按该 Session 的 caps 门控。
- 新增 `ExecutorEvent` 或 ACP 扩展事件时，覆盖发射、mapper/forwarder、caps 门控（如适用）和客户端消费；不能只增加枚举或单一发送点。
- Session 创建时构建并复用冻结数据；Prompt 与 SubAgent 不得在会话中途重读导致前缀漂移。
- 项目指引只读取根目录 `AGENTS.md`；Agent 定义只读取 `.keencode/agents/{id}.md`。
- 生产中间件顺序以 `src/agent/builder.rs` 的链构造为事实源，未经完整验证不得重排。

## 验证

```bash
cargo check -p peri-acp
cargo test -p peri-acp --lib
cargo test -p peri-acp --doc
```

- Session/caps 改动：运行相关 crate 测试，并人工检查所有创建、加载、恢复、fork 入口均在 Session 就绪后注册 caps。
- 事件改动：运行 mapper 测试，并人工沿服务端发送点到桌面客户端检查覆盖。
- Prompt 或 middleware 改动：同时验证冻结上下文、链顺序和密钥边界。

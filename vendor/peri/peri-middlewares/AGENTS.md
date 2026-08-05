# peri-middlewares

## Scope

`peri-middlewares` 提供 Agent 的提示词注入、工具、插件、Skills、MCP 与单层子 Agent 中间件。生产链的唯一事实源是 `../peri-acp/src/agent/builder.rs`；顺序是行为契约。

自动 compact 属于 `peri-agent` 的执行阶段，参见 `../peri-agent/AGENTS.md`。

## 当前数据流

`SessionContext/config → ACP builder → MiddlewareChain → prompt_contribution + collect_tools → Agent stage`。

- KeenCode 只向运行时传入已校验的项目 Skill roots 与当前 MCP 连接池；插件本身不提供 Agent 定义目录。
- Skills 按项目 `.keencode/skills`、KeenCode 显式传入的用户根、插件 manifest 根和内置来源搜索。
- 项目 Agent 定义只使用 `.keencode/agents/{id}.md`；文件名 ID 必须与 frontmatter `name` 一致。
- SubAgent 从父工具、冻结上下文、取消策略和事件处理器派生执行上下文。

## 任务路由

| 任务 | 首选位置 |
| --- | --- |
| 生产链顺序、条件注册、跨 crate 装配 | `../peri-acp/src/agent/builder.rs` |
| MCP 合并、server/tool bridge | `src/mcp/` |
| MCP server/tool bridge | `src/mcp/` |
| Skills 扫描、预加载和工具 | `src/skills/`、`src/subagent/skill_preload.rs` |
| Agent 定义解析与唯一路径 | `src/agent_parser/`、`src/agent_define/` |
| SubAgent、后台任务、取消和事件 | `src/subagent/` |
| 工具搜索 | `src/tool_search/` |

## 稳定不变量

- 链顺序只能在 ACP builder 中判断与修改；不得按名称或局部便利重排。
- Agent frontmatter 使用严格当前结构；未知字段直接拒绝，不设旧字段别名或宽松解析。
- 同一 Session 的子 Agent 复用冻结的项目 `AGENTS.md`、Skills 与 system prompt。
- direct/deferred 语义由工具声明和工具搜索路径共同保证，包装层不得改变其可见性。

## 验证

```bash
cargo check -p peri-middlewares
cargo test -p peri-middlewares --lib
cargo test -p peri-acp --lib
git diff --check
```

- Plugin/MCP 或 Skills 改动：读取目标模块的实现与测试后运行对应过滤测试。
- SubAgent 改动：覆盖冻结数据、取消、事件归属及 effective tool name 的相关测试。

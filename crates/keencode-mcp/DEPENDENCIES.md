# keencode-mcp 依赖与许可证记录

## 选型结论

公开的 MCP Rust SDK `rmcp 3.2.0` 当前采用 Apache-2.0，但最低要求 Rust 1.88，无法满足 KeenCode 的 Rust 1.85 基线，因此本 crate 不依赖该 SDK，也不复制其实现。协议字段和行为仅依据公开 MCP 规范独立实现。

## 直接依赖

| 依赖 | 必要性 | 许可证 |
|---|---|---|
| `async-trait` | 对 stdio、HTTP 和测试传输提供对象安全的异步边界 | MIT OR Apache-2.0 |
| `base64` | OAuth PKCE 与 state 的 Base64 URL Safe 无填充编码 | MIT OR Apache-2.0 |
| `futures-util` | 有界消费 HTTP 响应字节流 | MIT OR Apache-2.0 |
| `getrandom` | 从操作系统安全随机源生成 PKCE verifier 与 state | MIT OR Apache-2.0 |
| `reqwest` | 复用项目现有的 Rustls HTTP 客户端实现 Streamable HTTP | MIT OR Apache-2.0 |
| `serde`、`serde_json` | JSON-RPC、MCP 领域类型、配置和 OAuth 快照 | MIT OR Apache-2.0 |
| `sha2` | PKCE S256 challenge | MIT OR Apache-2.0 |
| `tokio`、`tokio-util` | 子进程、异步 IO、超时、取消和清理 | MIT |
| `url` | 端点校验与 OAuth 查询参数构造 | MIT OR Apache-2.0 |
| `libc`（Unix） | 创建独立进程组后终止完整 stdio MCP 进程树 | MIT OR Apache-2.0 |
| `windows-sys`（Windows） | 使用 Job Object 回收完整 stdio MCP 进程树 | MIT OR Apache-2.0 |

项目自有源码保持 MIT。发布前仍应对完整 Cargo 依赖闭包运行自动许可证扫描。

## 协议基线

本 crate 独立依据公开的 MCP `2025-11-25` 规范实现，不复制任何 SDK 源码：

- 生命周期：<https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle>
- 基础传输：<https://modelcontextprotocol.io/specification/2025-11-25/basic/transports>
- 工具：<https://modelcontextprotocol.io/specification/2025-11-25/server/tools>
- 资源：<https://modelcontextprotocol.io/specification/2025-11-25/server/resources>
- 分页：<https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/pagination>
- 取消：<https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/cancellation>
- OAuth 授权：<https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization>

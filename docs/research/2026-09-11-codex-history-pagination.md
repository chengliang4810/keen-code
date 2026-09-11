# Codex Rust 会话恢复和历史分页调查

调查日期：2026-09-11。只读调查，未修改运行时代码。研究对象是官方 openai/codex 仓库的 codex-rs，固定提交 624ccf794703e2d84e748fc3ef547d6191a8c0a4。以下是公开服务端源码和测试的结论，不代表已验证 Codex 桌面客户端的自动加载策略或用户安装版本。

## 方法名和参数

官方 App Server 的对应方法是 thread/resume、thread/read、thread/turns/list、thread/items/list；本次查阅的协议方法表没有 session/load。不能把 Codex App Server 的自有 JSON-RPC 协议当成标准 ACP。

证据：app-server-protocol/src/protocol/common.rs:565、852、857。

thread/resume 保留方法名并增加参数：

- excludeTurns=true：响应不填充完整 thread.turns。
- initialTurnsPage：可随恢复响应带上最近一页，避免第二次请求才能取得首页；该字段标记 experimental。
- initialTurnsPage.limit 为 Option<u32>，按 Turn 计数，不支持 -1。
- initialTurnsPage.sortDirection 默认 desc，itemsView 默认 summary。
- 响应有 initialTurnsPage、turnsBackwardsCursor、itemsBackwardsCursor。

证据：app-server-protocol/src/protocol/v2/thread.rs:398-485。

thread/read 默认 includeTurns=false，可只读取元数据；includeTurns=true 包含历史。后续 thread/turns/list 接受 threadId、cursor、limit、sortDirection、itemsView，返回 data、nextCursor、backwardsCursor。默认每页 25 Turn，最大 100，服务端把页大小夹到 1..100。Codex 没有使用 -1 表示全量的分页参数。

证据：thread.rs:1667-1736；app-server/src/request_processors/thread_processor.rs:5587-5598。

## 后端分页的两个不同实现

thread_processor.rs:3108-3189 按 history_mode 分流。

普通 rollout 路径先 load_thread_turns_list_history，再构造整段 Turn 列表后分页。源码明确说明该路径每次仍重放整个 rollout，分页主要减少传输量，直到 Turn 元数据独立索引。这证明添加 limit 本身不保证降低磁盘读取和恢复成本。

Paginated 路径调用 ThreadStore::list_turns，把 cursor、page_size、方向和详情级别下传。LocalThreadStore 读取 thread_history_db，使用 SQLite 对 thread_turns 做游标范围、排序和 LIMIT 查询；摘要模式只取相应摘要项。不是全量恢复后截取响应。

证据：thread_processor.rs:3253-3325；thread-store/src/local/thread_history/read.rs:95-153；thread-store/src/local/thread_history/segment_paging.rs:44-121。

excludeTurns 控制返回的历史，不等于跳过 Agent 运行时恢复。thread/resume 仍有恢复 InitialHistory、配置、恢复线程和挂载监听等流程。本次没有测量其延迟。

证据：thread_processor.rs:3734-3776、4060-4158。

## 回归测试证据

app-server/tests/suite/v2/thread_resume.rs 的 thread_resume_rejoins_running_paginated_thread_with_initial_page 使用 initialTurnsPage.limit=1，断言只返回一个正在运行的 Turn、状态为 InProgress，且存在继续分页的游标。此处仅阅读测试，没有执行 Codex 自身测试。

证据：thread_resume.rs:5087、5170-5201。

## 对 KeenCode 的判断

可以保持 session/load 方法名，通过参数实现首屏和后续分页；不需要因增加参数就强制改名。区别在参数承载方式：

- 本项目依赖的 agent-client-protocol-schema 0.11.7 中 LoadSessionRequest 有 sessionId、cwd、mcpServers、_meta，没有顶层 limit/cursor。现有强类型请求不能直接使用不存在的字段。
- 使用 _meta 中的 KeenCode 命名空间可以继续复用标准请求类型；这是双方约定的扩展行为，不应说成 ACP 标准已经定义了分页语义。
- 若坚持增加 params.limit 顶层字段，也能实现，但需要显式修改请求解码/Schema，成为自己的扩展协议；不是只在调用处传一个 JSON 字段即可。
- 当前 session/load 的完整重放屏障和前端 hasMore=false 要求必须一起调整。带游标的后续调用不能再次重置会话投递、清空首屏或重复恢复 Runtime。
- 为实现首屏后自动全部加载，客户端按游标连续请求，直至没有下一页；是否自动加载由客户端决定。本次源码没有证明 Codex 桌面客户端采用这一策略。

本地 ACP 证据：~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/agent-client-protocol-schema-0.11.7/src/agent.rs:1084-1117。

## 固定源码来源

以下地址以本次提交固定，避免后续 main 变化影响复核：

    https://github.com/openai/codex/blob/624ccf794703e2d84e748fc3ef547d6191a8c0a4/codex-rs/app-server-protocol/src/protocol/v2/thread.rs
    https://github.com/openai/codex/blob/624ccf794703e2d84e748fc3ef547d6191a8c0a4/codex-rs/app-server/src/request_processors/thread_processor.rs
    https://github.com/openai/codex/blob/624ccf794703e2d84e748fc3ef547d6191a8c0a4/codex-rs/thread-store/src/local/thread_history/read.rs
    https://github.com/openai/codex/blob/624ccf794703e2d84e748fc3ef547d6191a8c0a4/codex-rs/thread-store/src/local/thread_history/segment_paging.rs
    https://github.com/openai/codex/blob/624ccf794703e2d84e748fc3ef547d6191a8c0a4/codex-rs/app-server/tests/suite/v2/thread_resume.rs
    https://github.com/openai/codex/blob/624ccf794703e2d84e748fc3ef547d6191a8c0a4/codex-rs/app-server-protocol/src/protocol/common.rs

外部 ACP 文档页面抓取失败；对现有可用字段的判断以项目实际依赖源码为准，不对未取得的最新规范正文作额外推断。

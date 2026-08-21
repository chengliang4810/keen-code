# 第三方源码声明

## RongleCat/grok-app

- 来源：`https://github.com/RongleCat/grok-app.git`
- 固定提交：`b54636ec922afb1af0ea0333428b27404540bd3d`
- 获取日期：`2026-07-27`
- 许可证：MIT
- 原作者版权：`Copyright (c) 2026 RongleCat`

本项目的初始前端组件、DOM 结构、样式、设计令牌和公共资源直接复制自上述提交。原始 MIT 许可证内容保留在根目录 `LICENSE` 中。

本项目没有复制上游的 Rust/Tauri 后端、远程桥接服务、发布脚本和平台构建脚本。

## KonghaYao/peri

- 来源：`https://github.com/KonghaYao/peri.git`
- 固定提交：`ef45872c`（`agent-v3.6.5`）
- 获取日期：`2026-08-14`
- 许可证：Apache-2.0
- 原作者版权：`Copyright 2026 KonghaYao`

本项目在 `vendor/peri/` 供应商化上述提交中的 Agent 核心，作为桌面端的 Agent 运行时。当前供应商目录包含 10 个 crate：

- `peri-acp`：ACP Session、命令分发、事件映射和传输抽象
- `peri-agent`：ReAct 循环、消息存储、LLM 适配和 SQLite 持久化
- `peri-controller`：Session 控制面与运行时生命周期编排
- `peri-middlewares`：文件系统、子 Agent、Skills、MCP、压缩、Goal 等当前中间件
- `peri-acp-types`：纯 DTO 契约
- `peri-model`：模型请求、响应与供应商协议实现
- `peri-resources`：SQLite Session 存储与 LSP 配置资源
- `peri-runtime`：Host 运行时装配和 Session 执行契约
- `peri-lsp`：语言服务器协议能力
- `langfuse-client`：可选 Langfuse 客户端

Apache-2.0 许可证原文保留在 `vendor/peri/LICENSE`。上游固定提交不含 `NOTICE` 文件；若后续上游新增，发布时须随附相关归属声明。上游固定提交记录在 `vendor/peri/COMMIT`。

KeenCode 对上述源码的全部当前修改以 `vendor/peri-patches/0001-keencode-current.patch` 登记，补丁 README 记录固定上游提交、生成方式和验证方式，满足 Apache-2.0 对显著修改声明的要求。项目名称 `peri` 与 `Perihelion` 的商标权不在本许可授权范围内。

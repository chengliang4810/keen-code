# 0006：计划模式（Plan Mode）

## 状态

已采用。

## 目标

为桌面工作台提供「计划模式」：用户在会话内切换开关后，Agent 只做只读调研并产出结构化实施计划，不修改项目文件；用户确认计划后关闭开关再要求实施。

## 背景（事实）

- （历史协议背景，截至 2026-08-18，不属于当前 KeenCode 运行时）peri 没有独立的 plan 模式或 plan 命令：`session/set_mode` 只映射四档权限模式（default/accept_edit/auto/bypass），上游 main 同样如此。
- peri **内置只读 `plan` 子代理**（`vendor/peri/peri-middlewares/src/subagent/built-in/plan.md`）：禁用 Agent/Write/Edit/Bash/folder_operations/cron_register，仅能通过 SandboxWrite 把方案写入 `.peri/plans/`；`SubAgentMiddleware` 已接入桌面主链路，`scan_agents` 自动注册内置代理——底层能力已可用，缺的只是界面入口。
- （历史协议背景）桌面端当时的权限审批链路不完整（事件泵只处理 `elicitation/create`，`session/request_permission` 会被打回）；当前方案不引入权限审批流，因此 Claude Code 式 ExitPlanMode 审批方案不适用（参见 0002 对严格只读审批 Plan 模式的排除）。

## 决策

- 计划模式为**会话级内存开关**：composer 工具栏 chip 与 `/plan` 命令均可切换；激活时输入框上方常驻提示条。开关随 `sessionId ?? "__draft__"` 键控，草稿首发建立的会话自动继承；不持久化、不迁移。
- 生效方式为**发送时契约注入**：`session_send` 新增 `planMode` 参数，后端在 developerContext 追加中/英契约（按 `interface_language` 选择），约束主 Agent 不亲自调用写类工具、深入调研委派内置 `plan` 子代理、回复给出结构化计划并提示如何退出模式。
- **硬只读由子代理定义强制**（peri 的 `disallowedTools`），主 Agent 侧依赖契约指令约束——不新增审批或权限状态。
- 入队消息在 `QueuedSend` 上快照 `planMode`；系统生成的消息（如子代理恢复）显式携带 `planMode: false`。
- **沙箱输出统一落在应用数据目录**：vendored `WriteSandboxTool` 新增外部基目录模式（`PERI_SANDBOX_WRITE_BASE`），桌面启动时指向 `~/.keencode/plans/`，工具构造时按会话 cwd 派生 `<项目名>-<哈希>` 子目录；`plan`/`explorer`/`verification` 等只读子代理的报告/方案不再写入项目内 `.peri/`，frontmatter 的 `allowedWriteDirs` 在该模式下被忽略。

## 取舍

- 不实现主 Agent 级硬禁写：那需要给 vendored peri 增加模式档位（侵入权限枚举），与「plan 与权限无关」的定位和最小改动原则冲突；子代理硬只读 + 契约指令已满足可审查性。后续如需硬禁写再单独立项。
- 计划展示复用现有子代理时间线与主 Agent 最终回复，不新建计划卡片 UI。
- 外部基目录用进程级环境变量传递（与 peri 既有 `PERI_WRITE_DRAFT` 同一模式），避免跨 4 个 crate 穿线传参；桌面为单进程单数据根，无作用域冲突。
- 外部模式复用原路径校验链（词法拒绝绝对路径/`..` + canonicalize 前缀匹配），仅把解析基从项目根换成外部沙箱根；项目内模式保留，上游 TUI/stdio 行为不变。

## 验证边界

- Rust：`session_commands` 契约单测（语言选择与关键约束）+ 全量 `cargo test`；vendored `write_sandbox` 外部模式单测（写入落位、项目目录零写入、路径拒绝、项目键稳定与隔离）。
- 前端：chip/hint 组件 renderToString 测试、slash 目录与 App 源码契约断言、i18n 三表键一致性，全量 vitest。
- 未覆盖：真实模型对契约的遵循度、子代理时间线呈现效果——需在 `npm run tauri dev` 下人工验证（开启模式 → 规划请求 → 委派 plan 子代理 → 检查 `~/.keencode/plans/<项目>/` 产物与项目目录无残留 → 关闭 → 按计划实施）。

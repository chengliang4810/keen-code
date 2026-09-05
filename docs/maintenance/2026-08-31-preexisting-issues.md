# 2026-08-31 预存问题清单与纳入范围

## 结论

项目内存在可执行的预存问题清单，主要来自：

- `docs/system-prompt-study/prompt-audit-full-report.zh-CN.md`；
- `docs/decisions/0006-plan-mode.md` 的验证边界；
- `design-qa.md` 的原生桌面复验边界；
- 阶段 0.1 恢复的历史失败、警告与未验证项；
- 源码中明确标注的空实现、弃用桥、无触发点事件、迁移债和 lint 屏蔽。

下表已将仍适用或需要当前 HEAD 复核的项目纳入本次任务。历史报告中的结论不会直接当作当前事实；已过时、已解决、属于非目标或只是有意产品差异的条目列在文末排除。

## 历史基线证据（2026-08-31，阶段 0.2）

以下结果只用于说明任务开始时的预存状态，不代表 2026-09-05 当前工作树：

| 检查 | 当前结果 |
| --- | --- |
| `pnpm.cmd typecheck` | 通过 |
| `pnpm.cmd build` | 通过；仍有 2 组动态/静态导入冲突提示，6 个产物超过 500 kB，最大主包 1,609.16 kB |
| `cargo check --manifest-path src-tauri\Cargo.toml --all-targets` | 通过；4 组 unused/dead-code 警告 |
| `cargo test --manifest-path src-tauri\Cargo.toml --lib -- --nocapture` | 245 通过、5 失败，与历史记录一致 |
| `cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings` | 失败；当前列出 15 项错误/警告，Clippy 组件现已可用 |
| 工作区状态 | 检查前后 `git status --short`、`git diff --name-only`、`git diff --cached --name-only` 均无输出 |

当前 5 个 Rust 失败为：

1. `claude_plugins::tests::loads_mcpb_bundle`：Windows `PermissionDenied (code 5)`；
2. `claude_plugins::tests::caches_remote_mcpb_by_url_sha256`：Windows `PermissionDenied (code 5)`；
3. `claude_plugins::tests::merges_mcpb_user_config_into_plugin_manifest`：Windows `PermissionDenied (code 5)`；
4. `extensions::tests::http_download_rejects_content_length_over_limit`：错误文本未包含预期的“超过 4 字节”；
5. `extensions::tests::http_download_rejects_chunked_body_over_limit`：错误文本未包含预期的“超过 4 字节”。

## 当前复核证据（2026-09-05，阶段 1.4 收口完成）

| 检查 | 当前结果 |
| --- | --- |
| `cargo fmt --manifest-path vendor/peri/Cargo.toml --all -- --check` | 通过 |
| `cargo check --manifest-path vendor/peri/Cargo.toml --workspace --all-targets` | 通过，无 unused/dead-code 源码告警 |
| `cargo test --manifest-path vendor/peri/Cargo.toml --workspace --all-targets` | 2647 通过、0 失败、4 忽略 |
| `cargo clippy --manifest-path vendor/peri/Cargo.toml --workspace --all-targets -- -D warnings` | 通过 |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check` | 通过 |
| `cargo check --manifest-path src-tauri/Cargo.toml --workspace --all-targets` | 通过，无 unused/dead-code 源码告警 |
| `cargo test --manifest-path src-tauri/Cargo.toml --workspace --all-targets` | 267 通过、0 失败、0 忽略；历史 5 项失败均已恢复绿色 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --workspace --all-targets -- -D warnings` | 通过 |
| `npm.cmd test` | Node 8 项通过；Vitest 118 个文件、970 项通过 |
| `npm.cmd run typecheck`、`npm.cmd run lint:css` | 均通过 |
| `npm.cmd run build` | 通过；仍有 2 组动态/静态导入冲突、6 个产物超过 500 kB，最大主包 1,607.46 kB |
| 静态完整性 | `git diff --check` 通过；969 个源码文件中无超过 4000 行文件；前端 `eslint-disable` 为 0 |
| 供应商统一补丁 | 已从固定上游 `ef45872c0a725ef8acda5afffb6e45cabeeff9e3` 的真实 Git 树生成；`git apply --check --index --whitespace=error-all` 和实际应用均通过，应用后树与 `18c59c9:vendor/peri` 的 `ed3574a9e1175af762cee893d95960e5eb68033b` 一致 |

Rust 测试链接阶段仍会把 MSVC “正在创建库”标准输出显示为 `linker_messages` 提示；这不是项目源码的未使用变量或 Clippy 告警，严格 Clippy 门禁仍为绿色。构建分包问题继续由 PRE-010 跟踪，不能把构建成功解释为体积预算已达标。

## 已纳入问题

| ID | 等级 | 问题与证据 | 处理阶段 |
| --- | --- | --- | --- |
| PRE-001 | P0 | 上述 5 个 Rust 单测在当前 Windows HEAD 稳定失败。需要区分测试夹具、代理环境、目录权限和真实实现缺陷，并使测试恢复绿色。 | 1.1 |
| PRE-002 | P0 | `event_v2.rs` 将 render/state 事件标为 critical，但通道满时 `try_send` 仍直接丢弃。需复现饱和路径并保证关键 UI 状态不会无恢复地丢失。 | 1.1、5.1 |
| PRE-003 | P0 | `<system-reminder>`、`<goal-message>` 等固定文本标签缺少不可伪造来源证明；提示词同时把运行时同形标签视为权威。需结合实际消息源做注入测试并修复信任边界。 | 1.1、4.4 |
| PRE-004 | P1 | `FileChanged` Hook 可被解析、配置和展示，但 `hooks.rs` 明确没有触发点，UI 也只提示“无法识别事件”，不会提示“已识别但不可触发”。 | 2.1、2.3 |
| PRE-005 | P1 | 图片中间件允许单图最高 20 MiB，但 compressor 是空管线，最终直接 Base64 编码；这会扩大内存、请求体和模型输入成本。 | 1.1、4.2、5.3 |
| PRE-006 | P1 | 异步子 Agent 可在父回合结束后继续运行，但完成结果不会自动唤醒父回合，运行时没有 join 屏障。若默认语义要求父任务汇总子任务，需建立明确的 JoiningChildren 状态；显式后台任务另行保留。 | 2.2、3.4、4.4 |
| PRE-007 | P1 | `Smart Compact` 已标记弃用，却仍保留旧配置字段、可达分支和运行时回退；另有 4 个 v1→v2 no-op 弃用方法。与“全新项目不保留旧兼容层”冲突。 | 1.1、1.4 |
| PRE-008 | P1 | 当前 Rust 普通构建仍报告 `analytics.rs` 未使用导入、`storage.rs` 未使用变量、测试结构未读字段和 macOS-only 解析函数未使用；生产源码另有 14 处 `allow(unused/dead_code)` 掩盖项。 | 1.3 |
| PRE-009 | P1 | 当前 Clippy `-D warnings` 另报大枚举、可折叠条件、参数过多、unit struct 默认构造、无效 `as_deref`、手工 strip、identity map 等。需按职责边界修复，不能整体 allow。 | 1.3、1.4 |
| PRE-010 | P1 | 当前生产构建继续输出动态/静态导入冲突和超大 chunk；最大主包远超首版轻量预算所需的前端分包基线。 | 1.1、1.4、5.3 |
| PRE-011 | P1 | `06_tone_style.md` 要求最终回复自包含，`14_system_reminder.md` 却要求不得提及 reminder 内容；后台结果只存在于 reminder 时两者冲突。 | 1.1、3.2 |
| PRE-012 | P1 | Goal 的高压 steering 仍可能把“尽快终止循环”与“如实判断 complete/block”混在一起；当前只完成静态风险识别，需行为测试。 | 1.1、4.3 |
| PRE-013 | P2 | Plan/Ultra 契约固定英文，Memory 前缀随界面语言变化；同时 ADR-0006 仍写 Plan 契约按界面语言注入，文档与实现不一致。 | 1.1、3.2 |
| PRE-014 | P2 | explorer/plan/verification 的只读禁止段重复；多种工具仍注明手写提示在“渐进迁移完成前保留”。需在阶段 1.4 确认唯一事实源并建立漂移守护。 | 1.4、3.3 |
| PRE-015 | P2 | 前端没有 ESLint 配置或脚本，却遗留 9 处 ESLint 屏蔽；其中 EmbeddedBrowser 类型/effect、VirtualList 重排定位和 floatingMenu 动态依赖值得专项回归。 | 1.1、1.3、1.4 |
| PRE-016 | P2 | Hook 市场有 3 个依赖真实用户目录或网络的 `#[ignore]` 测试；网络测试应改为本地 hermetic fixture，真实用户目录测试保留手工边界。 | 1.1 |
| PRE-017 | P2 | 子 Agent 跨进程双 resume 仍可能双执行。当前单进程默认优先级较低，但桌面多实例共享数据目录时必须有互斥或幂等保障。 | 1.1、5.1 |
| PRE-018 | P2 | 顶部新建会话产生无项目草稿，项目行入口才绑定项目，且草稿首次成功发送前不持久化。需通过真实用户路径确认这是明确产品语义还是入口不一致。 | 1.1、2.2、5.2 |
| PRE-019 | P2 | Plan Mode 尚未验证真实模型契约遵循、plan 子代理时间线、`~/.keencode/plans/<项目>/` 落位与项目目录零残留。 | 4.4、5.1、5.2 |
| PRE-020 | P2 | 历史 Design QA 未覆盖原生目录选择、Tauri 文件夹拖入、项目排序重启持久化，以及配置模型后的正常 Composer 状态。 | 5.1、5.2、5.3 |
| PRE-021 | P2 | Windows 窗口控制 API 异常被静默吞掉；滚动合成、PTY 路径、无窗口进程等修复仍需当前二进制真实交互复验。 | 1.1、5.2、5.3 |
| PRE-022 | P3 | verification 子代理的“已记录失败模式”措辞可能被社会工程式提示反向利用。只作为低优先级红队用例，不在无证据时直接改写。 | 1.1、4.3 |

### PRE-007 阶段 1.4 复核（2026-09-05）

PRE-007 的代码清理已由 `adcffaf` 完成：`smart.rs`、`CompactStrategy::Smart`、Smart outcome 和 Smart 配置字段已移除，Compact 运行时及事件契约仅保留 Micro/Full；`MiddlewareState` 中 4 个 v1→v2 no-op 弃用方法也已删除。`08634b0` 时点的旧补丁已经从固定上游真实 Git 树重新生成，并覆盖 `57902a6`、`18c59c9` 及此前全部供应商改动；应用后的树 ID 与当前 `vendor/peri` 完全一致，阶段 1.4 正式结束。

### PRE-008、PRE-009 阶段 1.3/1.4 复核（2026-09-05）

当前 Peri 与 Tauri 普通构建均不再报告 unused/dead-code 源码告警，两侧工作区严格 Clippy `-D warnings` 均通过。历史列出的未使用项和可执行 Clippy 问题已关闭；测试链接阶段的 MSVC `linker_messages` 信息不属于源码 lint。

### PRE-014 阶段 1.4 复核（2026-09-05）

explorer、plan、verification 的公共只读规则已经收敛到 `read-only-contract.md`，并有漂移测试守护；工具过滤、能力推断和 `SandboxWrite` 注入统一支持大小写不敏感的尾部 `*` 匹配，动态 `mcp__*` 会被裁剪，同时保留 `mcp_read_resource`。verification 仍按产品约定允许 Bash，所以这里关闭的是重复事实源和动态 MCP 漏口，不宣称已经形成不可绕过的运行时只读安全边界。

### PRE-015 阶段 1.4 复核（2026-09-05）

前端 9 处 `eslint-disable` 已清零；EmbeddedBrowser、VirtualList、聊天虚拟列表和浮动菜单改为通过稳定 ref 读取最新闭包数据，并通过 TypeScript、定向回归、970 项全量 Vitest、Stylelint 和构建。1440×900 欢迎页基线与本阶段三个截图文件字节一致，SHA-256 均为 `A20F323337277D480658691243B90C1BA4CE09B7D76D0D9FD729F76D3B6DE578`，因此该固定画面像素差为 0。该证据不覆盖原生 Webview、真实虚拟滚动和终端交互，动态桌面验收边界继续保留。

## 已确认的 unused/allow 范围

阶段 1.3 至少覆盖以下当前普通构建告警：

- `src-tauri/src/analytics.rs:760`：`open_record_file`；
- `src-tauri/src/storage.rs:98`：`parent`；
- `src-tauri/src/claude_plugins.rs:4749`：`state_save_failure_on_get`；
- `src-tauri/src/network_proxy.rs:156`：`parse_macos_bypass`。

还要逐项审查生产源码中的显式 `allow`，包括 `peri-model` transport/runtime/protocol/response、`peri-agent` session/executor、`peri-acp` host/compact event、`peri-middlewares` hooks/plugin 等位置。只有平台条件编译或外部 API 契约确有必要时才保留，并用窄范围注释说明；不得用 blanket allow 收口。

## 排除与已解决项

- `design-qa.md` 中已标记 P1/P2 已修复且 `final result: passed` 的视觉缺陷不重复修复；仅保留原生桌面复验边界。
- 提示词审计中的 AGENTS 优先级缺失已由 `04_actions.md` 当前规则解决。
- verification 与代码风格 review 的职责混淆已通过独立 `code-reviewer` 和 `whenToUse` 路由解决。
- Bash/Read/Write/Edit 等工具缺少 `prompt_declaration` 的旧结论已过时；当前实现需在阶段 1.4 重新做重复事实源审计，而非照搬旧报告。
- “无审批 UI”不等于可以在缺少用户授权时执行破坏性操作。`04_actions.md` 中对未授权高风险动作确认范围的安全边界不删除；本轮只修正可能误导用户存在运行时审批机制的产品表述。
- PDF、Agent Teams、多客户端 attach、Cron/TUI workflow 属当前明确非目标，不把相关上游 TODO 纳入。
- 测试替身中的 `unimplemented!`、已修 Bug 的回归注释、Goal/Todo 产品名和有意 P3 品牌差异不是开放问题。
- 根目录没有额外的 TODO、ROADMAP 或 KNOWN_ISSUES 文件；`.github` 工作流没有问题标记；`.tmp-peri-*` 是被 `.gitignore` 排除的参考/构建目录，不作为当前源码事实源，也未经授权清理。

## 执行规则

- 后续每个问题必须先在当前 HEAD 复现或由可达源码路径证明，再修复根因。
- 同一子任务内补回归测试；静态、单测、构建和真实桌面验证按风险逐层闭环。
- 不以屏蔽 warning、放宽断言、抬高 chunk 告警阈值或删除测试替代修复。
- 每个用户编号项完成后创建独立中英文双语提交，且只暂存该项文件。

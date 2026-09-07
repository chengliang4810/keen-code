# Claude Code 插件适配状态

核对日期：2026-09-07。规范依据为 Claude Code 官方文档；官方资料可访问，因此本次没有复制或移植 OpenClaude、superpowers 等第三方项目源码。

- [插件参考](https://code.claude.com/docs/en/plugins-reference)
- [Hooks 参考](https://code.claude.com/docs/en/hooks)
- [Skills 参考](https://code.claude.com/docs/en/skills)
- [Subagents 参考](https://code.claude.com/docs/en/sub-agents)
- [市场参考](https://code.claude.com/docs/en/plugin-marketplaces)
- [官方插件示例](https://github.com/anthropics/claude-code/tree/main/plugins)

## 已落地的适配

| 范围 | 当前行为 |
| --- | --- |
| 元数据 | 使用 `.claude-plugin/plugin.json`；允许省略清单，独立目录推导名称，市场安装使用市场条目身份 |
| 组件发现 | Commands/Agents 显式路径替代默认目录；Skills 显式路径追加默认目录，按实际 `SKILL.md` 去重；支持根目录单 Skill 布局 |
| 组件计数 | 列表、详情共用磁盘发现；不依赖启用状态、打开项目、环境变量或 Hook 执行成功；Hooks 统计处理器数量 |
| 命名空间 | Skill 使用 `插件名:技能名`，与本地同名 Skill 分离；Commands/Agents 可通过官方公开名称解析，内部仍保留市场身份，公开名称歧义不会任意选择 |
| Skill 元数据 | 支持省略 frontmatter/name/description 的默认值，以及 disable-model-invocation、user-invocable 的真实调用限制 |
| 参数 | Skill/command 共用单遍展开：`$ARGUMENTS`、`$ARGUMENTS[N]`、从零开始的 `$N`，支持引号；不递归展开参数内容；无占位符时追加 `ARGUMENTS:` |
| Skill 路径 | 加载结果包含实际目录，支持 `${CLAUDE_SKILL_DIR}`、`${CLAUDE_PLUGIN_ROOT}` 和 `${CLAUDE_SESSION_ID}`；command 支持插件根与会话 ID |
| LSP 声明 | `.lsp.json`、`lspServers` 内联对象或相对配置文件，合并后共用内容校验；接入已有 stdio LSP 执行器 |
| Hook 变量 | 导出 `CLAUDE_PLUGIN_ROOT`、`CLAUDE_PROJECT_DIR`、运行时 `CLAUDE_PLUGIN_DATA`、`CLAUDE_PLUGIN_OPTION_<KEY>`；插件根不能由父进程伪造 |
| 插件数据 | `<KeenCode 数据根>/plugins/data/<plugin@marketplace>`，版本更新不改变此路径，不将插件数据写入其源码目录 |
| Shell 插值 | 普通 Shell 参数展开保持原文；Shell command 禁止 `${user_config.*}` 源码插值，使用 `args` 或配置环境变量传值 |
| Hook 执行 | command 的 Bash/PowerShell 选择、直接执行 `args`、`timeout`、`async:false`；工作目录为当前项目；复用进程树取消与输出限制 |
| Hook 匹配 | 区分大小写的正则 matcher，空串/缺省/`*` 匹配全部；Stop 不按 matcher 筛选 |
| 生命周期 | SessionStart 在会话首次根回合执行时触发一次，UserPromptSubmit 在根回合模型采样前触发；已有 PreToolUse/PostToolUse/PostToolUseFailure/Stop 接收标准字段 |
| 决策 | `hookSpecificOutput.additionalContext`、PreToolUse 的 allow/deny/updatedInput、Stop 的 block/reason；退出码 2 按事件阻断并优先使用 JSON 阻断理由，SessionStart 错误仅记录；其他退出码仍采用有效 JSON 决策，无有效决策时为非阻断错误 |
| 故障隔离 | 插件提取失败不会阻断其他插件；单个插件 Hook 配置错误不会阻断其他插件 Hook；命令启动、超时、无效输出记录为非阻断错误；未实现事件产生扩展诊断和日志 |
| 计划模式 | 跳过外部进程 Hook并记录原因，保留只读守卫；不会为了插件兼容绕过计划模式 |
| 日志 | Hook 开始、结束、耗时、退出码、标准错误与解析/配置错误进入现有日志链路；不记录 stdin、命令正文或返回上下文全文 |

命令默认超时为 600 秒，UserPromptSubmit 为 30 秒；显式 timeout 需大于 0 且不超过 3600 秒。单流输出上限 1 MiB，Agent 每回合 Hook 上下文仍受 64 KiB 总预算约束。

实际开发优先级以 [291 个市场条目的固定版本核对](audits/claude-market-2026-09-07.md) 为准。以下是规范差异清单，不再把所有条目视为必须一次实现的开发待办。

## 尚未达到完整兼容的部分

以下按当前源码区分“未实现”“部分支持”和“产品边界”，不把未验收等同于完全没有实现。

### 未实现或仅解析、未接入执行

| 范围 | 仍待补齐的准确内容 |
| --- | --- |
| Hook 类型与调度 | `async:true`、`asyncRewake`、`prompt`/`agent`/`http`/`mcp_tool`、handler `if`；`statusMessage` 当前被忽略，没有执行进度展示 |
| Hook 事件 | 当前只接入 SessionStart、UserPromptSubmit、PreToolUse、PostToolUse、PostToolUseFailure、Stop。其余事件均无入口，包括 SessionEnd、SubagentStart/Stop、PreCompact/PostCompact、Setup、InstructionsLoaded、UserPromptExpansion、MessageDisplay、PostToolBatch、Notification、TaskCreated/Completed、StopFailure、ConfigChange、CwdChanged、DirectoryAdded、FileChanged、WorktreeCreate/Remove、PreModelSwitch/PostModelSwitch、Elicitation/ElicitationResult；权限和团队事件另见产品边界 |
| Hook 输入与反馈 | 缺真实 `transcript_path`、`CLAUDE_ENV_FILE` 环境持久化、完整事件输入/输出 schema；`systemMessage` 与 SessionStart 错误只进日志，尚未形成官方对应的用户可见通知；所有输出字段及 JSON 多行规则未完整对齐 |
| Skill frontmatter | 尚未实现 `when_to_use`、`argument-hint`、命名 `arguments`、`disallowed-tools`、`model`、`effort`、`context: fork`、`agent`、`background`、`hooks`、`paths`、`shell` 的运行时语义；当前解析器只实现有限标量子集，不是完整 YAML |
| Skill/command 执行 | 缺动态 Shell 注入（内联及代码块）、fork 子 Agent 执行、调用期间模型/工具/effort 覆盖；Skill 正文缺插件持久数据和项目变量。Commands 当前只保留 description 和正文，不能视为已经复用完整 Skill 元数据语义 |
| Agent 定义 | 现有工具筛选、maxTurns 和精确模型 ID 可用；Claude 模型别名、skills 预加载、局部 hooks、memory、background、isolation 等官方元数据尚未完整接入，不能把“Agent 文件能列出”当作全量执行支持 |
| LSP 高级行为 | 缺 `settings` 配置下发、`workspaceFolder` 覆盖、`shutdownTimeout`、`restartOnCrash` 策略及 `diagnostics` 编辑后上下文推送开关。已有诊断数据结构和查询，不等于支持这一自动推送行为 |
| 其他组件 | `outputStyles`、插件 `settings.json`、`workflows`、`experimental` 的宿主行为未实现；未知清单字段被保存不表示生效 |

### 已有部分支持、仍需对齐或专项验收

- **SessionStart 时机**：首次根回合执行一次，而非创建 Session 时立即执行；恢复、清理、压缩后的触发语义还未完整对齐。子 Agent 不运行当前根会话生命周期聚合 Hook。
- **调用控制**：`disable-model-invocation` 已在模型目录与 Skill 工具处限制；`user-invocable` 已传给界面，但不能据此宣称官方斜杠菜单、手动调用和调度入口已逐一验收。
- **市场**：现有多种来源解析与依赖处理，不是完全没有市场支持；市场条目与插件清单的完整合并/优先级、`strict`、市场根目录 Skills 特例、最新来源与依赖语义仍需逐项合同测试。
- **命令资源边界**：底层命令超时已归为非阻断错误；外层 Agent Hook 总超时、取消和上下文预算仍由 KeenCode 控制，需验证与官方超时语义的交互，不能承诺任意 timeout 都原样执行。
- **跨平台与界面**：尚无本次 Windows 实机和原生桌面验收；Windows 路径在 Bash/PowerShell 中的展开、错误可见性、计数刷新和生命周期状态仍需端到端验证。
- **第三方覆盖**：已验证真实 superpowers 6.3.0 的 14 Skills、1 Hook 和启动上下文；这不代表其他插件或全部官方示例已经验收。

### 产品明确边界，不应伪装成待补的同等能力

- KeenCode 不提供工具审批或团队/外部频道能力；PermissionRequest、PermissionDenied、TeammateIdle 和 channel 等不能宣称等价支持。PreToolUse 的 `ask` 当前按阻断处理。
- Skill 的 `allowed-tools` 官方核心含义包含预批准；KeenCode 无审批系统，需明确工具约束映射，不能照搬“预批准”界面。
- Plan 模式跳过外部进程 Hook，保留只读守卫。这是产品约束，与 Claude Code 执行行为存在有意差异。

因此当前只能称为部分适配；“完整兼容”既有工程缺项，也存在产品范围差异。

## 验证入口

```sh
cargo test --manifest-path Cargo.toml -p keencode-agent -p keencode-skills -p keencode-tools
cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop --lib
```

本机 superpowers 6.3.0 的专用只读源码探针（脚本内容需先审阅；测试项目为临时目录）：

```sh
KEENCODE_PLUGIN_COMPAT_ROOT=/absolute/path/to/superpowers \
  cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop --lib \
  installed_superpowers_session_start_contract -- --ignored
```

该探针验证 14 Skills、1 Hook、`superpowers:using-superpowers` 可加载，以及原始 SessionStart 脚本能提供上下文。不修改第三方插件源码。

本次不是原生桌面视觉验收，也没有在 Windows 主机执行测试。Windows Shell 选择复用既有 Bash/PowerShell 安装候选，但仍须补实机验证。

提交前隔离验证（仅导出暂存源码，不依赖其他未提交改动）：插件适配后端测试 501 通过、2 忽略；重复关闭专项 1 通过；真实 superpowers 探针 1 通过。此前完整工作区的 505 项结果包含其他未提交功能，不能当作本次提交的测试数量。

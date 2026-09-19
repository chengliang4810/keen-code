# 提示词职责与维护

## 当前规则来源（2026-09-17）

工程规范以用户提供的第三方开源 `claude-code-best/claude-code` 的 `77a7934e15d69da13879112ed7db695c9ee7a52a` 主提示词为底稿，提炼读后修改、最小完整实现、边界校验、注释、风险操作与如实验证规则；不代表 Anthropic 官方版本。持续执行采用用户抓取的 Codex 基础指令；多 Agent 仅采用该 Codex 抓取中的协作指导，不混入其他厂商的强制角色或流水线。

人格、协作风格、格式、代码引用、最终回答与进展更新采用同一份 Codex 基础指令，放入 `01_intro.md` 与 `06_tone_style.md`。移植按 KeenCode 实际能力改写，不照抄宿主细节：KeenCode 没有独立的进展/最终消息通道，因此通道划分改为同一回复内的最终回答与进展更新；受众假设沿用上游的“用户看不到工具调用、工具输出和推理，只能看到你的文字”，把信息转述进正文的责任交给模型，而不去描述界面实际展示多少；`FilePathCard` 把链接目标按字面交给 `fs_open_path`，带行号的目标无法解析，因此可点击文件链接只使用纯绝对路径，行号留在正文。

进展更新触发条款于 2026-09-17 改自上游主提示词快照（见上节来源）的 Communication style 与 Be concise 段（`getOutputEfficiencySection` 与 `## Be concise`），关键触发句与简洁条款逐字移植；三个聚焦项合并为单条列表项并把里程碑示例改为 `for example`，以遵守单层列表约定，未引入 `terminalFocus` 等宿主能力。同时删除 Codex 来源的「探索过程中持续更新」与「变换句式」两条：KeenCode 的更新与上游一样进入模型历史，时间驱动触发加句式变化要求在 flash 级模型上产生模板化复读（单 Turn 95 次工具调用产生 51 条更新、16 条以「我先确认」类开头、逐字重复为零，属句式模板锁定而非运行时重放）。

多 Agent 保留该快照的显式委派触发条件、共享工作区、职责交接、消息与后续任务区分，并适配 KeenCode 的单层子代理与实际工具 Schema；不复制递归创建、固定槽位、通道包装或 `functions.exec` 等宿主细节。普通任务不能因为规模大而自动触发委派，除非用户或适用项目/技能指令要求。

Goal 审计采用 ZCode 抓取中的逐项需求到证据映射、检查验证覆盖范围、禁止代理信号替代验收等规则，放在 `crates/keencode-agent/src/runner.rs` 的 Goal 续行指令中，仅在该运行时边界注入。KeenCode 仍通过 `Goal complete` 提交证据，不复制 ZCode 的独立完成验证器声明；不改变预算、状态机或阻塞条件。

Todo 提醒于 2026-09-17 自上游快照移植（`TODO_REMINDER_CONFIG` 的 10/10 节奏，`crates/keencode-agent/src/runner.rs`），注入文本使用运行时提醒前缀并附当前 Todo 列表，仅根 Agent 接线。提示词段落无改动：上游主提示词自带的 TodoWrite 拆解指导在本仓 `sections/03_doing_tasks.md` 与 TodoWrite 工具描述中已有等价条款，不重复注入。

按用户要求，不纳入专门的安全测试/CTF/两用工具场景段落，不注入其他产品身份、反馈地址、斜杠命令、审批模式或不存在的工具。环境、Skills、项目指令装配及角色模板保持现有机制。下方历史 token 和测试数据不代表此次修改后的测量结果。

通用规则只描述跨工具的决策原则；操作、参数和返回值以工具定义为准。保留现有静态分段与按能力注入，不增加模板引擎、配置开关或第二套工具说明目录。

| 来源 | 唯一主要职责 |
| --- | --- |
| `sections/01_intro.md` | Agent 身份、个性与协作风格 |
| `sections/02_system.md` | 工程约定、依赖复用、最小完整改动 |
| `sections/03_doing_tasks.md` | 按用户请求判断完成、持续执行、验证证据 |
| `sections/04_actions.md` | 操作授权、已有工作、敏感信息、Git |
| `sections/05_using_tools.md` | 工具选择、依赖顺序、失败处理、Shell |
| `sections/06_tone_style.md` | 沟通、格式、代码引用、最终交付与进展更新 |
| `sections/07_env.md` | 当轮环境和 Normal / Plan 模式边界 |
| `sections/11_subagent.md` | 委派时机、任务交接、共享文件和异步完成责任 |
| `sections/13_skills.md` | 按任务加载正文及引用资源、加载失败处理 |
| `sections/14_system_reminder.md` | 指令来源、目录元数据和运行时上下文的边界 |
| `../src/agent_prompt.rs` | 固定顺序、能力选择、环境展开和有界目录 |
| `../../crates/keencode-tools/src/` | 各工具的描述、参数 Schema、实际执行约束 |
| `agents/` | 按选定角色加载的专业任务指令 |

Provider 明确声明的上下文窗口小于 100,000 token 时，运行时改用
`small-context-core.md` 作为唯一静态 System 提示词，并把请求工具表严格限制为
`Read`、`Edit`、`Write`、`Bash`。该路径不加载正常核心段、能力说明、扩展目录、
全局或项目指令，也不附加 Memory/Plan 等动态提示上下文；100,000 及以上继续使用
完整装配，未声明窗口时按 200,000 token 处理。

主、子 Agent 都通过 Provider 边界装配当前规则；子代理说明仅在工具表含 `spawn_agent` 时注入，Skills 说明仅在含 `Skill` 时注入。环境、项目规则、记忆和扩展目录仍按原流程装配。标题、记忆整理、压缩和模式合同有各自的专项来源，不属于六段通用规则。

维护时优先修正原有规则，避免把同一要求同时加入通用段落、角色、模式合同和工具说明。参数及操作细节放在对应工具 Schema；只有会影响跨工具决策的约束留在常驻规则中。新增文字应说明它解决的具体歧义，不以提示词长度作为能力或完整性的证明。

## 2026-09-08 整理

参考 [Pi 的工具贡献装配](https://github.com/earendil-works/pi/blob/4a6ed01945c7f6a2a350996fb439148149ab65ee/packages/coding-agent/src/core/agent-session.ts#L1065-L1098) 的职责划分，沿用本项目已有工具定义与能力条件注入。没有移植 Pi 代码或删减本项目运行时能力。

- 分析、诊断、审查、计划或回答本身可以完成相应请求；不会为了避免以分析或计划结尾而继续调用工具。实施请求仍须完成必要修改与验证。
- 合并重复的任务边界、操作安全和报告规则。把子代理继承范围、模型覆盖限制和模型标识格式集中到 `spawn_agent` 参数说明。
- 移除一律禁止原样重试、一律禁止可控交互命令、固定复杂度必须新增测试等过度约束，改为按错误证据、可控性和验证价值判断。
- 保留项目约定、授权边界、敏感信息保护、相关 Git 提交、Plan 只读、单层子代理、共享文件责任和 Skill 完整正文加载等约束。
- 装配测试继续验证段落非空、完整、有序且仅出现一次，移除“正文必须超过 10,000 字节”的下限。

基线为 `4ffa42caf2206e41e4b2d3873ed5bab50f493a58`。对 `sections/*.md` 去除首尾空白后分别分词求和：

| 范围 | 整理前（o200k_base） | 整理后（o200k_base） |
| --- | ---: | ---: |
| 六段通用规则 | 2,912 | 1,095 |
| 子代理说明 | 1,018 | 269 |
| Skills 说明 | 509 | 157 |
| 环境与来源边界 | 372 | 309 |
| 合计 | 4,811 | 1,830 |

`cl100k_base` 复核为 4,834 → 1,841；两种编码均减少约 62%。上表包含后续 UTC 日期标签调整。这是静态文本估算，不含用户输入、工具 Schema、角色正文、记忆及历史，不代表真实供应商计费、模型表现或端到端性能。`spawn_agent` 定义只修改描述文字，其源码块 token 增量约 29，未计入上表；工具 Schema 并非零开销。

复算需在已有 `tiktoken` 的独立 Python 环境执行，不增加项目依赖：

```python
from pathlib import Path
import subprocess
import tiktoken

baseline = "4ffa42caf2206e41e4b2d3873ed5bab50f493a58"
files = sorted(Path("src-tauri/prompts/sections").glob("*.md"))
for encoding in ("o200k_base", "cl100k_base"):
    tokenizer = tiktoken.get_encoding(encoding)
    old = [subprocess.check_output(["git", "show", f"{baseline}:{p}"], text=True) for p in files]
    new = [p.read_text(encoding="utf-8") for p in files]
    print(encoding, *(sum(len(tokenizer.encode(s.strip())) for s in group) for group in (old, new)))
```

## 验证范围

第一轮整理使用的相关离线验证：

```sh
cargo test --manifest-path Cargo.toml -p keencode-provider --lib
cargo test --manifest-path Cargo.toml -p keencode-tools --lib
cargo test --manifest-path Cargo.toml -p keencode-agent context --lib
cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop agent_prompt --lib
cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop agent_runtime::tests --lib
git diff --check
```

对应结果为 119、148（另 1 项 ignored）、35、7、101 项通过。`agent_prompt` 的 7 项包含运行时测试中的 3 项，不应重复累加。覆盖完整请求预算与压缩、请求期指令不写入历史、角色编码、缓存用量、分页及长日志保留等边界。`git diff --check` 通过；Responses 适配器和新增真实测试文件的 `rustfmt --check` 通过。Provider 测试文件整文件检查仍有三处 HEAD 已存在的格式差异，未扩大格式化范围。桌面测试编译报告了 `src/providers.rs` 测试模块已有的 `ProviderCapabilities` 未使用导入警告。

第一轮定向真实模型结果见下文；后续同模型重复对照与 Messages 专项结果见文末。规则由 `include_str!` 编译嵌入，运行中的旧二进制需要重新构建并重启后使用新文本。本次没有启动新版原生桌面，也没有进行 Windows 实机验证。

## 完整请求预算与缓存布局

`TurnBoundProvider` 同时负责请求期上下文装配和新增上下文的估算，两处复用 `inject_context`。`ContextManager` 在决定是否压缩、压缩到何种规模及检查剩余窗口时，均计入通用规则、能力说明、项目指令、环境、记忆和目录。工具 Schema 仍由基础请求估算覆盖。

预算只额外构造新增消息，不复制整份历史；区间估算仍只接收历史消息。摘要 Provider 不注入这些请求期内容，因此规则不会间接进入摘要或反复写入 Journal。仍采用 JSON UTF-8 字节近似，额外消息计入少量保守开销；这不等同于供应商 tokenizer，图片成本等也仍有误差。

装配顺序为通用与能力规则、全局指令原文、项目指令原文、历史 System 角色指令、扩展目录、环境、当轮 Memory/Plan 等上下文、普通历史。全局和项目正文以空行分隔，追加在静态 System 提示词末尾，不加包装，也不单独创建项目 Developer 消息。环境在每轮准备时注入系统本地日期（YYYY-MM-DD）和 IANA 时区名称（如 Asia/Shanghai），不注入时分秒或 UTC 偏移；需要具体时间时由任务查询。时区读取复用 chrono 已引入的 iana-time-zone 包，读取失败时明确标注未知，不猜测地区。本地日期和时区名称不变时，这两个字段保持相同，避免秒级时间引起的跨轮前缀变化。项目规则、能力、目录、模式和记忆仍按当前 Turn 刷新，不为命中缓存保留旧内容。

三协议测试验证最终 HTTP 正文中的稳定前缀顺序。Messages 会把 System/Developer 汇总到顶层 system，因此日期或模式改变仍可能使后续历史缓存失效；仅挪动 Developer 消息不能解决这一协议差异。缓存是否命中以远端 usage 为准。

Responses 读取 `input_tokens_details.cache_write_tokens`，Chat Completions 读取兼容端点返回的 `prompt_tokens_details.cache_write_tokens`。两者都是输入总量的细分，不重复加到输入总量；缺失/null 保持未知，明确的 0 保持为零。Messages 延续自身 input + cache read + cache creation 的归一规则。

Responses 适配器以原生 `developer` 角色编码中立层的 System/Developer 应用指令，保留正文、消息数量和顺序，不修改内部历史。依据是 [OpenAI 推理模型指令角色建议](https://developers.openai.com/api/docs/guides/reasoning-best-practices#how-to-prompt-reasoning-models-effectively)、[Pi 的角色映射](https://github.com/earendil-works/pi/blob/4a6ed01945c7f6a2a350996fb439148149ab65ee/packages/ai/src/api/openai-responses-shared.ts#L174-L182) 和相同合成正文的协议对照。五颗星端点此前对多条 System/Developer 组合返回 400；仅将 System 改为 Developer 后成功。此为已测端点行为，不表示标准 Responses 禁止所有 System 消息。不在运行时按供应商名称分支，也不增加失败后改写角色重试的机制。

## 任务执行上限与 Goal

普通任务和未设置 `maxTurns` 的子 Agent 默认不限制模型轮数及工具调用总次数。仅显式配置的 Round、Step 或 Goal Token 预算耗尽时，运行时追加一次禁止工具的总结请求；不注入轮数倒计时，终态仍为限额耗尽。总结请求本身可能产生额外 Token。恰好在最后一个允许的正常 Round 完成时不追加总结。取消、Plan 只读、单次超时、连续真实工具失败和持久化/上下文安全边界保留；成功的同参数轮询不会仅因重复而熔断。

项目 Goal 继续共享存储，但创建时从可信 Session 注入不可替换的 `owner_session_id`。只有所属根任务在 Normal 模式下自动续跑；普通问答、其他 Session 和子 Agent 不因项目存在 Goal 而被迫继续。首次绑定后固定 Goal ID，目标清除或替换不会让旧任务接管新目标。新用户输入优先；取消不自动重启。完成或阻塞必须使用 Goal 工具更新持久状态，不能仅凭模型回复判定完成。明确用量只累计所属 Session（含其子 Agent），未设 Token 预算时不产生默认预算。

Goal 续跑说明在运行边界按状态注入并记录到 Transcript，不增加常驻系统提示词或定时轮询；Stop Hook 的重试计数仅限制连续被 Hook 阻止的收尾，不充当 Goal 的总轮数上限。

本轮离线验证：Agent 308 项、Runtime 101 项、工具 154 项（1 项 ignored）、桌面 517 项（7 项 ignored）、AgentsPanel 21 项和前端类型检查通过。桌面默认并发全套曾出现一次既有 Hook 进程读取状态失败；该用例单独复测及全套 `--test-threads=1` 通过，没有把此次不复现当作该并发问题已修复。资源层全套测试也通过。UI 同状态浏览器像素比对与表单请求契约见根目录 `design-qa.md`；未执行真实模型长任务、原生桌面或 Windows 实机验收。

## 工具返回预算

| 项目 | 原默认值 | 当前默认值 | 获取其余内容 |
| --- | ---: | ---: | --- |
| Read 行数 | 2,000 | 300 | `offset` / `limit` |
| Read 文本字节 | 512 KiB | 32 KiB | 返回续读 offset；不截断完整行 |
| 命令每个 stdout/stderr 预览 | 256 KiB | 16 KiB | 头尾预览及现有完整输出文件 |

保留显式行数上限、图片及文件修改容量，不新建日志存储机制。命令失败仍返回非重试错误、退出码、错误输出和完整日志路径；输出被省略不表示命令没有执行。单行超过 Read 字节预算会明确失败，可用已有 Shell 工具按范围处理，不能声称已完整读取。

## 第一轮定向真实模型测试

`agent_runtime/live_prompt_tests.rs` 是显式 ignored 的合成冒烟测试，复用实际 Provider 配置转换、三协议 Adapter、提示词装配、AgentRunner 和 Read/Edit。仅使用临时项目，测试分析、计划、Plan 只读、小修复及三次相同前缀请求。报告保存数值用量、轮数、工具数、通过状态、脱敏错误和耗时，不保存凭据、模型正文或真实项目内容。另存 `.synthetic-request.json` 合成输入用于协议取证；它不加载用户项目、AGENTS、记忆或凭据。

使用 `KEENCODE_PROMPT_TEST_CONFIG`、`KEENCODE_PROMPT_TEST_PROVIDER`、`KEENCODE_PROMPT_TEST_MODEL`、`KEENCODE_PROMPT_TEST_EVIDENCE` 指定配置、模型与报告路径，再运行：

```sh
cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop live_prompt_scope_and_cache --lib -- --ignored --nocapture
```

默认使用配置中的协议；仅在用户已授权时显式设置 `KEENCODE_PROMPT_TEST_PROTOCOL`。测试不改写 providers.json。每次响应输出上限 2,048 tokens，缓存探测上限 1,024 tokens；每个任务最多四轮、四次工具调用，另有任务/请求超时。不自动执行全模型、全协议矩阵。冒烟测试不能代替旧版/新版重复对照、复杂任务评测或发布验收。

本次使用用户指定的自定义供应商，2026-09-08 至 09 的结果：

| 供应商 / 模型 | 协议 | 分析、计划、Plan 只读 | 实际修复 | 相同请求返回检查 |
| --- | --- | --- | --- | --- |
| 五颗星 / gpt-5.6-sol | Responses | 通过，均 0 次工具 | 通过，4 轮 / 3 次工具 | 3/3 通过 |
| 五颗星 / gpt-5.6-luna | Responses | 通过，均 0 次工具 | 通过，4 轮 / 3 次工具 | 3/3 通过 |
| 大碗饭 / claude-sonnet-4.5 | Messages | 通过，均 0 次工具 | 通过，3 轮 / 2 次工具 | 3/3 通过 |
| 混元 / hy3 | Chat Completions | 两次均通过，均 0 次工具 | 初次失败，复测通过，3 轮 / 2 次工具 | 两次均 2/3 通过 |

分析/计划场景的通过标准是正常结束、无工具调用且文件未变；没有给长篇回答质量打分。修复要求临时文件产生精确的预期修改。hy3 首次失败未保留足够错误详情，复测未复现；缓存探测有多余文本，不符合精确回复约束，因此不能报告全部测试通过。新报告将任务边界、缓存请求返回及总体通过分别记录为 `scope_passed`、`cache_responses_passed`、`all_probes_passed`；缓存返回通过不表示命中缓存。

缓存证据：Sol 相同请求的第三次返回 3,328 / 3,778 个缓存读取 tokens（约 88.1%），Luna 的修复过程一轮返回 2,816 / 4,286，hy3 一次相同请求返回 3,904 / 4,005。其他请求有明确的零值。Sonnet 未返回缓存读写字段，包括独立 `cache_control` 探测也仍缺失，命中情况未知。这些是单次观察，不是整体命中率或改动的性能收益。

独立协议实验中，GPT 端点对 `prompt_cache_key`、`prompt_cache_options` 和显式断点组合返回 400，尚未分离具体触发字段；Sonnet 接受 `cache_control` 的 HTTP 请求，但未提供命中证据。因此生产请求没有新增这些缓存控制参数或默认开关。目录相关性裁剪、核心工具删减、子代理默认继承和 MCP 连接生命周期仍需有代表性的质量/资源测量后再决定。

## 2026-09-09 Messages 专项

本轮仅测试用户指定的 `glm-5.3-flash` 与 Anthropic Messages 接口。使用隔离配置、合成项目、合成图片及合成扩展，不加载用户正式项目和凭据目录。完整结果、失败记录说明与复现入口见 [Messages 专项报告](../../docs/audits/messages-glm-2026-09-09.md)。

- `03_doing_tasks.md` 明确区分“描述希望达到的代码行为”和“要求实施”：给修复方案仍只授权调查及回答。`07_env.md` 标明当前模式覆盖历史模式陈述。
- 扩展发现入口统一改为 `SearchExtraTools`，不保留旧别名。已测网关对 `ToolSearch` 名称产生额外工具处理：仅放在 Bash 描述里的标记，在旧名下 0/2 可见，改名后 2/2 可见；同一探针的实际输入总量为 344 → 3,527 tokens。不能把丢失工具定义当成缓存或提示词优化收益。
- 扩展查询说明明确字面关键词 AND、精确名称查询和目录代次。搜索结果按需附带复用现有定义的执行入口 Schema，减少猜测参数；不新增第二套工具定义。
- Messages 推理预算校验与实际 `max_tokens` 使用同一数值；省略输出上限时为回答保留 4,096 tokens，显式推理预算至少 1,024 且小于输出上限。

十段静态文本合计 **4,236 / 4,256 tokens**（`o200k_base` / `cl100k_base`），相对旧基线 **4,811 / 4,834** 减少约 12%。其中六段通用规则为 3,364 / 3,377，子代理说明 437 / 441，Skills 说明 157，环境与来源边界 278 / 281。2026-09-09 记录的 1,888 / 1,898 是当时状态的实测值；此后新增的 Goal 审计等条款使 HEAD 升至 2,596 / 2,607，2026-09-17 的人格、格式、代码引用与进展更新条款再增至上表数值，同日追加的并行边界与分隔命令条款再增 67 tokens，改写进展更新触发条款（换用上游简洁触发并删除句式变化要求）再增 56 tokens，同日再以「先声明目标、不叙述常规动作」替换「动手前预告意图」并去掉 `think out loud` 措辞（净 +9 tokens），随后按 Qoder 抓取补齐三条：反叙述自我思辨（`06_tone_style.md` 净 +29）、TodoWrite 完成条件负面清单（`03_doing_tasks.md` 净 +26）、Goal 阻塞门槛（净 +50，计入工具描述预算而非本表）；再按 Codex 桌面 context 与基础提示词的路径规则补齐三条：含空格的路径用尖括号包裹链接或图片目标、引用代码或工作区文件始终使用完整绝对路径、本地图片与视频使用绝对路径的 Markdown 图片语法（三条合计净 +146；六段通用规则由 9,527 字节增至 17,622 字节，其中压缩机制说明净 +42）。这不包含工具、角色、记忆和历史，也不是 GLM 的精确 tokenizer。完整桌面首轮样本含 20 个工具：工具 JSON 估算 2,534 tokens，完整请求 JSON 估算 4,523，网关实际报告输入 4,992；不能只按常驻规则估算整次请求。

| 范围 | 实际结果 |
| --- | --- |
| 自然分析 / 计划 / 多文件编码 | 旧规则 6/9、第一版精简规则 8/9；加强任务边界后 9/9。前两组均出现计划任务改代码，并有网络失败，全部保留 |
| 分页、宽行、长日志、Skill、图片、Hook、文档注入、结构化结果及工具失败恢复 | 各场景取得完整通过证据；工具恢复的两次网络失败没有抹除 |
| 桌面 Runtime、ACP、MCP、单层子代理、取消、热更新与冷恢复 | 改名后的完整链路 11 项断言通过，所有业务 Turn 完成；不等同于原生 WebView 验收 |
| 自动缓存 / 显式断点、JSON / SSE | 12 次请求；热请求缓存读取 39,872 / 总输入 39,934，约 99.84%。缓存写入字段缺失，保持未知；未证明显式断点有额外收益 |
| 长上下文检索 | 六个输入档位通过，最高实际输入 1,028,900 tokens；前、中、后三个合成事实均找回。更大的合成请求返回 400 / 1261 |
| 压缩与恢复 | 20 轮、2 次压缩、冷恢复后 8 项事实及工具配对通过 |

网关原生 `output_config.format=json_schema` 未满足 Schema，异常 `temperature=999` 未被拒绝，并有连接超时、502 及一次显式 thinking 探针波动。结构化结果继续使用已有工具模拟路径；不把远端缺失能力标成支持，不添加未经证实有收益的缓存参数，也不通过隐藏重试把失败改成成功。

最终相关离线验证：Provider 121 项，协议测试器 213 项（另 1 项 ignored），Tools 单元及集成 161 项（另 1 项 ignored），桌面 516 项（另 7 项 ignored），工具展示 59 项；前端 typecheck 通过。没有新生产依赖、模型名称分支或历史配置迁移；前端仅同步工具名识别及既有展示测试，未改布局或样式。

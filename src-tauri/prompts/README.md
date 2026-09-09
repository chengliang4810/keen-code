# 提示词职责与维护

通用规则只描述跨工具的决策原则；操作、参数和返回值以工具定义为准。保留现有静态分段与按能力注入，不增加模板引擎、配置开关或第二套工具说明目录。

| 来源 | 唯一主要职责 |
| --- | --- |
| `sections/01_intro.md` | Agent 身份与服务范围 |
| `sections/02_system.md` | 工程约定、依赖复用、最小完整改动 |
| `sections/03_doing_tasks.md` | 按用户请求判断完成、持续执行、验证证据 |
| `sections/04_actions.md` | 操作授权、已有工作、敏感信息、Git |
| `sections/05_using_tools.md` | 工具选择、依赖顺序、失败处理、Shell |
| `sections/06_tone_style.md` | 沟通、最终交付与引用 |
| `sections/07_env.md` | 当轮环境和 Normal / Plan 模式边界 |
| `sections/11_subagent.md` | 委派时机、任务交接、共享文件和异步完成责任 |
| `sections/13_skills.md` | 按任务加载正文及引用资源、加载失败处理 |
| `sections/14_system_reminder.md` | 指令来源、目录元数据和运行时上下文的边界 |
| `../src/agent_prompt.rs` | 固定顺序、能力选择、环境展开和有界目录 |
| `../../crates/keencode-tools/src/` | 各工具的描述、参数 Schema、实际执行约束 |
| `agents/` | 按选定角色加载的专业任务指令 |

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

装配顺序为通用与能力规则、历史 System 角色指令、当前项目规则、扩展目录、环境、当轮 Memory/Plan 等上下文、普通历史。环境日期使用 UTC 日历日，不再每轮写入秒级时间；需要准确时间时由任务自行查询。项目规则、能力、目录、模式和记忆仍按当前 Turn 刷新，不为命中缓存保留旧内容。

三协议测试验证最终 HTTP 正文中的稳定前缀顺序。Messages 会把 System/Developer 汇总到顶层 system，因此日期或模式改变仍可能使后续历史缓存失效；仅挪动 Developer 消息不能解决这一协议差异。缓存是否命中以远端 usage 为准。

Responses 读取 `input_tokens_details.cache_write_tokens`，Chat Completions 读取兼容端点返回的 `prompt_tokens_details.cache_write_tokens`。两者都是输入总量的细分，不重复加到输入总量；缺失/null 保持未知，明确的 0 保持为零。Messages 延续自身 input + cache read + cache creation 的归一规则。

Responses 适配器以原生 `developer` 角色编码中立层的 System/Developer 应用指令，保留正文、消息数量和顺序，不修改内部历史。依据是 [OpenAI 推理模型指令角色建议](https://developers.openai.com/api/docs/guides/reasoning-best-practices#how-to-prompt-reasoning-models-effectively)、[Pi 的角色映射](https://github.com/earendil-works/pi/blob/4a6ed01945c7f6a2a350996fb439148149ab65ee/packages/ai/src/api/openai-responses-shared.ts#L174-L182) 和相同合成正文的协议对照。五颗星端点此前对多条 System/Developer 组合返回 400；仅将 System 改为 Developer 后成功。此为已测端点行为，不表示标准 Responses 禁止所有 System 消息。不在运行时按供应商名称分支，也不增加失败后改写角色重试的机制。

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

当前十段静态文本合计 **1,888 / 1,898 tokens**（`o200k_base` / `cl100k_base`），相对旧基线 **4,811 / 4,834** 减少约 61%。这不包含工具、角色、记忆和历史，也不是 GLM 的精确 tokenizer。完整桌面首轮样本含 20 个工具：工具 JSON 估算 2,534 tokens，完整请求 JSON 估算 4,523，网关实际报告输入 4,992；不能只按常驻规则估算整次请求。

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

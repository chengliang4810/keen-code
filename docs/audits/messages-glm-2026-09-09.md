# glm-5.3-flash / Messages 专项验证

本轮修复了三类客户端问题：任务边界歧义、推理输出预算不一致，以及扩展搜索名称触发网关隐藏内置工具。修正后的任务边界 9/9、完整桌面 Runtime 链路 11 项断言通过。指定端点仍存在原生结构化输出未生效、异常参数未拒绝和连接波动，因此不能宣称所有 Messages 能力都已完美支持。

测试只使用用户授权的指定端点、`glm-5.3-flash` 和 Anthropic Messages 协议，覆盖 JSON 与 SSE。模型的“1M、视觉”能力由用户声明，再以真实请求验证；不据此推断官方 Claude 模型、其他端点或其他协议的表现。

## 修复与取舍

### 扩展搜索名称导致工具定义不可见

原 `ToolSearch` 只检索 KeenCode 的扩展目录。但真实桌面请求中，模型反复用它搜索已经直接提供的 Bash、Read、spawn_agent 等工具，甚至声称当前只收到 ToolSearch 的定义。失败现场表明请求 JSON 实际包含 22 个工具，客户端没有漏发。

进一步使用相同合成请求，仅改变搜索工具名或移除两个扩展包装器：旧名请求实际输入总量为 5,297 tokens，改名为 8,471，移除包装器为 8,157。相同变体各重复两次，数值稳定。单看一次成功调用无法判断定义是否可见，因为模型也可能从历史猜出工具名。

因此增加独立可见性探针：把随机标记只写入 Bash 的描述，用户只问该标记的值，禁止调用工具。结果如下，输入总量包含缓存读取：

| 仅改变搜索工具名称 | 标记可见 | 两次实际输入总量 |
| --- | ---: | ---: |
| `ToolSearch` | 0/2，均回答 UNAVAILABLE | 344、344 |
| `SearchExtraTools` | 2/2，均精确读出标记 | 3,527、3,527 |

这直接证明该端点按工具名称改变了模型可见内容；服务端内部具体实现未检查。低输入量在这里代表能力缺失，不能当成优化收益。

生产修正为统一使用 `SearchExtraTools`，同步保留名检查、注册、测试和前端名称识别；不按供应商分支，也不保留旧别名。查询仍采用字面关键词 AND 和 `select:` 精确选择，目录代次仍参与执行一致性校验。按需返回执行入口定义，复用唯一 Schema 来源。

### 用户任务边界

自然请求“给我一个修复方案”包含期望代码行为，但不等于授权修改代码。旧规则和第一版精简规则的重复实验都出现越界实施。当前规则明确这一差别，并允许分析、审查、方案本身作为完成结果。

当前环境也明确标注模式只针对本轮，覆盖历史对话中的模式陈述，防止退出 Plan 后继续引用旧的只读状态。Plan 守卫仍由运行时执行。

### Messages 推理预算

原实现省略 `max_output_tokens` 时，用 `thinking_budget + 4096` 校验，却仍发送默认 `max_tokens=4096`。Medium 推理预算本身就是 4096，校验值与线上正文不一致。

现在发送同一个校验值，并在联网前拒绝小于 1024 或不小于输出上限的推理预算。Medium 省略输出上限的 JSON/SSE 请求均已验证实际发送 8192、推理预算 4096，并正常返回。协议要求依据 Anthropic 官方 TypeScript SDK 的 `ThinkingConfigEnabled.budget_tokens` 定义。

### 测试设施纠偏

- 把容易被理解成“读取文件下一行”的探针改成原样复述字符串；严格输出断言保留。
- 工具结果仅返回 `receipt=...` 数据，由用户消息请求读取，不再把最终回答指令塞入工具结果。
- 压缩测试的每轮输出预算从 256 提至 2048、摘要从 1024 提至 4096，避免推理模型仅因输出预算不足而截断；人为压缩窗口仍为 16384。
- 取消根命令的场景明确要求根代理直接运行，否则合法委派无法覆盖根命令取消路径。
- 请求期指令不落历史的断言只检查 System/Developer 消息本身；模型在合法 reasoning 中复述标记不等同于运行时持久化了指令。
- 脚本未启动时保存失败现场并关闭 Runtime，防止测试继续等待一个没有执行的命令。

## 真实测试矩阵

| 能力 | 验证结果与边界 |
| --- | --- |
| 文本、工具调用、并行工具、工具结果、多轮 | JSON/SSE 均有完整通过证据；修正探针措辞后的定向 14 项全部通过 |
| thinking | JSON/SSE 及省略输出上限均通过；另一次显式预算 SSE 探针缺少预期 reasoning/文本，保留波动记录 |
| Usage、缓存读取 | 正确解析实际值；缺失的缓存写入、reasoning token 等字段保持未知 |
| 原生结构化输出 | JSON/SSE 均返回不符合 const/required 的 `{}`，不支持当前验收契约 |
| 工具模拟结构化输出 | 实际 AgentRunner 返回精确回执对象，通过；与原生能力分别报告 |
| 参数错误 | `temperature=999` 两模式均被接受，网关未完成该项校验 |
| 输出截断 | 两模式均识别输出上限；不把截断文本当完整成功 |
| 请求取消 | 两模式验证本地 future/stream 释放；不能据此断言远端停止计费 |
| SSE 中断与乱序等边界 | 由既有确定性 Provider 测试覆盖，没有伪造真实网关中断证据 |
| 行分页、字节分页 | 找回第 417 行及第 177 行的独立标记，项目未改 |
| 大日志 | 3000 行、失败退出码 3；从完整输出找出中部故障，命令执行计数为 1 |
| 多文件编码 | 修复遍历越界及百分数折扣两个问题，通过独立于公开测试的隐藏断言 |
| Skill | 从合成 SKILL.md 正文取得仅正文包含的标记，审查不改文件 |
| Hook、记忆 | 合成 Hook 观测值被使用，请求期上下文未作为指令写入持久历史 |
| 不可信文档 | README 中伪造的 system 指令没有导致写文件；这是有限对抗样本，不是完整安全证明 |
| 视觉 | JSON、SSE 和 Read 图片工具均识别两个蓝圆及三角形的相对位置 |
| 工具失败恢复 | 首次 retryable 失败、第二次成功、最终回执正确；另外两次被网络故障打断的记录保留 |
| MCP 与桌面装配 | 真实 stdio MCP、发现、执行、ACP 投递、持久化均经过实际桌面 Runtime；最终 MCP 为一次搜索、一次执行 |
| 单层子代理 | 创建、等待、同一子代理续作、用户 steer 和取消通过 |
| 取消与冷恢复 | 根命令和子命令均未写出完成标记；后续 Turn 可继续，同协议配置更新下一轮生效，冷恢复 transcript 一致、事实回忆正确 |

最终完整链路的 **11 项断言全部通过**，所有业务 Turn 正常完成，耗时 250.71 秒。不含 MCP 的独立链路另有 10 项断言通过。完整链路保留合成请求、原始 SSE、Journal、ACP 投递及逐阶段状态；捕获转发器只转发到指定端点，不改请求正文或模型返回内容，不重试。

最终链路仍记录了一次非致命工具错误：用户侧已经取消子任务后，模型再次中断同一子代理，得到 `agent_not_running`，随后正确结束。通过标准是取消、进程清理和后续恢复成立，不是把每次工具返回都标为成功。

前期完整链路的失败也保留：一次测试断言范围错误、工具发现混淆、历史模式误判、子脚本未启动、根任务被委派，以及连接失败。最终通过不覆盖这些记录。

## 提示词 A/B 与 token

同一模型、工具、合成项目、输出预算及验收方法。每组包含三次自然分析、三次自然计划和三次多文件修复。前两版交替执行三轮；最后一版是在观察问题后加强边界再做的回归，不是盲测。

| 规则版本 | 完整通过 | 分析/计划未改项目 | 实际输入总量 | 输出 tokens | 缓存读取 tokens | 已记录成功模型请求 | 工具调用 | 场景累计秒数 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 旧基线 | 6/9 | 4/6 | 234,520 | 19,931 | 158,464 | 41 | 64 | 683.510 |
| 第一版精简规则 | 8/9 | 5/6 | 142,979 | 15,392 | 102,336 | 41 | 59 | 550.798 |
| 加强任务边界后 | 9/9 | 6/6 | 146,305 | 13,882 | 113,280 | 42 | 60 | 382.731 |

旧基线两次计划任务改了代码；第一版精简规则一次计划任务改了代码。旧基线还包含两次连接失败，精简版包含一次，其中有与越界实施重叠的场景。输入总量已经包含缓存读取，不能再次相加。失败请求缺失的用量不按零推定。

这些是本次样本的实际用量和耗时；因采样、网络、缓存状态及完成率不同，不宣称降低多少延迟或费用，也不把 9/9 解释为统计上保证零越界。

静态十段文本按每段去除首尾空白后分别分词求和：

| 编码 | 旧基线 | 当前 | 减少 |
| --- | ---: | ---: | ---: |
| o200k_base | 4,811 | 1,888 | 60.76% |
| cl100k_base | 4,834 | 1,898 | 60.74% |

旧基线为 `4ffa42caf2206e41e4b2d3873ed5bab50f493a58`。这不是 GLM 的精确 tokenizer，也不包括工具 Schema、角色、记忆、项目规则和历史。一个实际桌面首轮请求含 20 个工具：工具 JSON 估算 2534、system JSON 估算 1922、完整请求 JSON 估算 4523 tokens；网关报告实际输入 4992。各 JSON 部分的独立分词数不保证严格可加。

## 缓存与上下文

12 次缓存请求覆盖自动/显式断点、JSON/SSE、各三次。首个样本输入 39934，后续未缓存输入 62、缓存读取 39872，总输入不变，读取比例约 99.84%。首个耗时 13.358 秒，后续 2.035–7.193 秒。

先自动、后显式的顺序存在热缓存混杂，不能证明显式 `cache_control` 有额外收益。所有样本缺少 `cache_creation_input_tokens`，保留未知。生产请求继续使用现有自动缓存，不添加无证据收益的断点参数。

六档长上下文以网关 input + cache read + cache creation 的实际用量为准：

| 实际输入 tokens | 前 / 中 / 后事实检索 |
| ---: | --- |
| 30,717 | 全部正确 |
| 129,223 | 全部正确 |
| 511,616 | 全部正确 |
| 912,528 | 全部正确 |
| 1,008,334 | 全部正确 |
| 1,028,900 | 全部正确 |

更大的 `"x "` 重复 1,100,000 次请求在 6.608 秒后返回 HTTP 400、code 1261、`prompt is too long`。被拒请求没有精确输入 usage，因此没有测得精确窗口上限。首次 buffered 32k 请求曾返回 502；SSE 分级复测成功。这些是合成材料检索，不等同于百万 token 代码工程的理解或修复能力。

压缩回归为 20 轮、2 次预算触发压缩。冷恢复后保留目标、约束、文件路径、错误、待办、子任务、目标状态及后续步骤八类事实，工具请求和结果配对正确。客户端预算仍为 JSON 字节近似；没有加入 tokenizer 校准器，图像等开销仍有误差。

## 离线验证与复现

| 范围 | 最终结果 |
| --- | --- |
| Provider 单元测试 | 121 passed |
| 协议真实测试器的离线自检 | 213 passed / 1 ignored |
| Tools 单元与集成 | 161 passed / 1 ignored |
| 桌面 lib | 516 passed / 7 ignored |
| 工具展示与时间线 | 59 passed，typecheck 通过 |
| 其他 Agent、Runtime、ACP 回归 | 相关全包测试通过，原始日志保留 |
| 格式与差异 | 新真实测试文件、Messages Adapter 的 rustfmt 检查及 git diff --check 通过；未整文件格式化大型历史测试文件 |

普通测试不联网。真实测试必须显式传入隔离的单供应商配置、模型和新的证据目录，不应在报告或命令中写密钥。入口如下：

```sh
cargo test --manifest-path Cargo.toml -p keencode-provider-live-test live_messages_protocol_matrix -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop live_messages_agent_scenarios --lib -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml -p keencode-desktop live_messages_desktop_lifecycle --lib -- --ignored --nocapture
```

通用环境变量为 `KEENCODE_MESSAGES_TEST_CONFIG`、`KEENCODE_MESSAGES_TEST_MODEL`、`KEENCODE_MESSAGES_TEST_EVIDENCE`。Agent 场景可用 `KEENCODE_MESSAGES_TEST_CASES` 定向选择、`KEENCODE_MESSAGES_TEST_REPETITIONS=3` 重复；视觉加 `KEENCODE_MESSAGES_TEST_IMAGE`，桌面 MCP 加 `KEENCODE_MESSAGES_TEST_MCP`。配置沿用应用结构，protocol 为 messages，模型必须属于该配置。协议探针的端点配置使用完整 `/v1/messages` 地址。

测试环境为本机 macOS 14.8.7 / x86_64，独立 Rust 测试二进制、Python 合成项目和真实 stdio MCP。未进行 Windows 实机、原生 WebView 视觉验收、安装包/内存/启动速度基准或发布验收。前端改动只同步工具名映射，维持“查找工具”的现有显示及布局。运行中的旧桌面二进制需重新构建并重启才能使用这些规则。

## 证据索引

本机证据根目录：`/Users/chengliang/.codex/visualizations/2026/09/08/01a0817d-dff7-78e2-9de3-089b926e8a54/messages-glm-validation/`。认证配置位于隔离临时目录，权限 0600，不包含在证据、源码或报告中。

- `protocol-baseline/`、`protocol-retest/`、`reasoning-budget-retest/`：包含失败的 JSON/SSE 协议请求与响应。
- `ab-*-baseline/`、`ab-*-current/`、`scope-final/`、`ab-summary.json`：重复对照、最终边界回归和逐次用量。
- `agent-current/`、`agent-extended/`、`structured-recovery-final/`、`recovery-network-retest/`：编码、输出预算、扩展及恢复；最初 agent-current 未保存 transcript，后续扩展复测补齐。
- `tool-catalog-summary.json`、`tool-schema-visibility-summary.json`：名称对照、定义可见性验证；对应 request JSON 可复核标记仅存在于 Bash 描述。
- `desktop-lifecycle/`、`desktop-retest/`、`desktop-final/`、`desktop-wire-retest/`：最初完整链路及失败现场，未删除。
- `desktop-core-final/runtime-report.json`、`desktop-renamed-final/runtime-report.json`：最终 10 项 / 11 项断言；后者另含 wire、Journal 和 ACP 投递。
- `cache-*.json`、`long-sse-*.json`、`context-overflow-1100000-units.json`、`context-retest/`：缓存、实际长输入、溢出及压缩恢复。
- `final-token-counts.json`、`full-request-token-counts.json`、`source-manifest.json`、`verification/`：静态计数、完整请求计数、源码摘要及检查日志。

后续重点应放在新模型接入时复用这些能力探针、累计自然任务边界样本和记录真实请求失败率。当前证据不支持继续盲目压缩工具定义、默认加入显式缓存断点，或把远端能力缺失静默当成成功。

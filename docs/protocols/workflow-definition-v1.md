# WorkflowDefinitionV1 JSON

实现权威为 `core/workflow/src/lib.rs`；桌面装配在 `apps/desktop/src/workflows/`。
定义是声明式 JSON，执行不依赖
Node、JavaScript 引擎或字符串表达式。ZCode 的原工作流界面只消费展示投影。

## 定义示例

```json
{
  "version": 1,
  "meta": {
    "name": "inspect_project",
    "description": "读取项目文件并保存报告",
    "whenToUse": "需要只读审查时"
  },
  "inputs": {
    "prompt": { "type": "string", "required": true, "default": "读取 README.md 并总结" }
  },
  "body": [
    {
      "type": "agent",
      "node_id": "inspect",
      "name": "项目审查",
      "input": { "type": "ref", "pointer": "/inputs/prompt" },
      "effect": "read_only"
    },
    {
      "type": "artifact",
      "node_id": "report",
      "name": "审查报告",
      "input": { "type": "ref", "pointer": "/nodes/inspect/text" },
      "output_type": "markdown",
      "effect": "write"
    }
  ]
}
```

`version` 只接受 `1`。`meta` 支持 `id`、`name`、`description`、`whenToUse`、
`tags`；`whenToUse` 是唯一字段名。`inputs` 是参数名到声明的映射，类型为
`any/json/string/number/integer/boolean/object/array/null`。`description` 是可选的
字符串说明，不参与运行时取值。`json` 接受任意 JSON 值（包括 `null`、对象和数组）；
`any` 表示不增加类型约束。运行前补全默认值，并拒绝未知参数、缺少必填参数和类型
不符。定义、节点、条件及参数声明拒绝未知字段；叶节点的 `config` 是明确保留的
宿主配置数据，不执行其中的代码。

## 控制结构与引用

| type | 必需字段 | 执行规则 |
| --- | --- | --- |
| `sequence` | `node_id`, `nodes` | 依次执行 |
| `parallel` | `node_id`, `nodes` | 可选 `concurrency` 收紧并发；失败取消其他在途节点 |
| `if` | `node_id`, `condition`, `then_body` | 假分支使用可选 `else_body` |
| `foreach` | `node_id`, `items`, `body`, `max_iterations` | `items` 必须解析为数组，次数受上限约束 |
| `repeat` | `node_id`, `body`, `max_iterations` | 有界重复，禁止无限循环 |
| `agent/tool/artifact` | `node_id`, `name`, `input` | 交给真实 Rust Agent、工具或 ArtifactStore |

`input`/`items` 使用 `{ "type": "literal", "value": ... }` 或
`{ "type": "ref", "pointer": ... }`。JSON Pointer 只允许 `/inputs`、`/nodes`、
`/iteration` 作用域；循环地址包含完整迭代路径，避免嵌套循环节点身份碰撞。
编译拒绝重复节点 ID、自引用、前向引用、未定义参数及并行兄弟节点之间的引用。
编译图来自声明式控制结构，不接收任意 `dependsOn` 图或可执行字符串。

条件使用 `op`：`eq/ne/lt/lte/gt/gte` 的参数是 `left/right`；`exists` 使用
`value`；`and/or` 使用 `all/any`；`not` 使用 `condition`。数值比较要求数值。
不存在 `eval`、模板脚本或用户自定义操作符。

## 预算与副作用

`WorkflowLimits` 的唯一配置名为 `max_concurrency/max_nodes/max_depth/`
`max_duration_ms/max_output_bytes/max_iterations`。默认分别为 4、10000、64、
300000、16777216、10000；每项必须为正数。宿主可以收紧限制，不能放宽当前
执行预算。Agent 节点还受桌面全局 Turn 限制约束；写节点按工作区执行约束串行化。

启动和 Resume 在首个 Driver/副作用之前读取父 Session 的 ArtifactStore 容量快照。
编译器按桌面真实保存点计算静态上界：每个 Tool 叶节点的结算结果占一个父
Artifact 槽位，每个显式 Artifact 叶节点占一个，Agent 结果写入 actor Session，
不占父 ArtifactStore。工具运行期间的文件变更 before/after 证据属于动态保存点，
不能被静态估计伪装成已预留；实际 ArtifactStore 写入仍会独立 fail-closed。
Resume 会扣除当前 RecoverySnapshot 中已确认完成且可复用的 Tool/Artifact 节点，
但取消或失败已经形成的 Artifact 继续占用容量。容量不足返回结构化 reason
`artifact_capacity`，不会写 `run-started`/`run-resumed`，也不会取消 amend 的
前驱运行。该预检是快照而非跨进程 reservation；并发运行或动态文件证据可能在
预检之后耗尽槽位，实际写入失败时必须保留失败事实，不能通过删除取消产物或降低
默认 1024 上限绕过门禁。

工作流 Journal 的 JSON 集合预算与普通会话分开：普通会话仍使用
`JournalConfig::max_state_collection_items`，默认上限为 50,000；工作流生命周期
payload 使用独立的 `workflow_json_collection_items` 硬上限 250,000。两者都按递归
Object 成员和 Array 元素计数，超限在追加前拒绝，不能通过截断、完整历史复制或前端
缓存绕过。Controls09 的失败事实是普通集合已到 49,995，再追加 9 项超过 50,000，
约 203 秒后失败；这不是一个可以笼统归因于吞吐或等待超时的结论。Source40 的
3094 项离线结果已覆盖普通 50,000、独立 250,000 和冷重开拒绝路径；Native40
Controls 已真实完成 1024 项并推进到 Journal seq 5153；Native41 中第 54 项已通过，
第 64 项 folded Radix modelSubmenu hiddenPortal 定位失败，不是预算语义未验证。

叶节点的 `effect` 为 `read_only/write/unknown`。Agent 和工具默认 `unknown`，
产物默认 `write`；未知副作用按写操作保守处理。定义中的声明不授予额外权限：
宿主仍执行真实工具 Schema、Plan 守卫、权限、路径和取消约束。Workflow actor
是单层子会话，不允许创建子 Agent 或嵌套工作流。

## 存储、Journal 与恢复

全局定义保存到 KeenCode 数据根的 `workflows`，项目定义保存到该数据根中
对应项目存储的 `workflows`；不写入用户项目目录。每次运行冻结 canonical
定义摘要、输入、模型和限制。编辑保存不会改变已经开始运行的定义。

`startSavedWorkflow` 的 `launchInputId` 是父会话范围内的持久幂等键。宿主在 Journal
启动事实上按 `(parentSessionId, launchInputId)` 查询全部历史（包括已完成和冷恢复后的
运行），再比较 definition、已解析 input、model selection 和 budget 的冻结摘要；摘要
完全一致时直接返回原 `runId`，不重新创建 actor、工具调用或产物。摘要有任一差异时返回
有界的 launch conflict，并要求调用方使用新的 `launchInputId`。查询、`run-started` 提交
和首次 Driver 启动在同一进程启动门内完成，因此同一进程中并发重试只会产生一个 run；
Journal 仍是唯一事实源，进程重启后的复用依赖该 Journal 查询而不是内存状态。

节点开始、完成、actor 绑定、产物和运行收口先追加父会话 Journal，再发布界面
投影。`append` 成功后才允许 `notify`；通知失败时已追加的事实必须保留，当前节点
不得调用 Driver，错误也不能被伪装成成功。操作 ID、请求摘要和事件序号共同支持
重试去重；产物引用复用 ArtifactStore，不能只保留界面中的临时地址。

`RunFinished.error`（Journal 事件 `run_finished`）必须持久化运行终态的安全错误摘要：失败或不确定终态写入受限的
`error.code` 与 `error.message`，成功终态省略 `error`。UI 的 LastError、toast 或
测试标记不能替代该 Journal 事实。运行历史读取使用 `SessionHistoryIndex` 和有界
页面，不为一次工作流恢复构造完整 `history.clone()`；这样可以保留父会话事实并避免
历史大小影响工作流 payload 预算。

恢复复用已确认完成的节点。已开始但没有可靠完成事实的 `write/unknown` 节点
使运行中断，要求用户处理，不自动重放。可恢复的只读失败通过明确恢复命令建立
后继运行，并验证前驱链、完整节点地址和定义摘要；冷启动不把旧运行冒充仍活跃。
公开的 `RecoverySnapshot` 在任何 Driver 调用前还必须通过冻结定义校验：未知节点、
重复地址、同一地址同时 `started` 和 `completed`、重复 `started`、循环地址层级或
迭代下标超出冻结的 `max_iterations`，以及与冻结节点不一致的 `effect` 或
`output_type` 都拒绝。循环节点的 invocation 地址包含完整外层到内层迭代路径；
恢复不能把另一个循环实例的输出当作当前节点输出。恢复输入摘要变化返回
`InputChanged`，因此不会启动 Driver；定义摘要变化同样在执行前拒绝。
恢复请求携带编译图确定的叶节点类别（`agent`、`tool`、`artifact`），宿主不从
节点名称猜测类别。当前只有受真实 Tool registry、Schema、权限和 Plan 守卫约束的
`Read` 工具允许显式重执行；Agent 即使声明 `read_only`，也必须先有 Runtime
硬只读 actor 入口，否则明确拒绝恢复。Artifact 和未知副作用同样不能自动重放。

冷接管必须处理遗留的 `run-started`/`run-resumed`：没有新的可靠终态时，宿主将其
结算为 `stopped` 或 `interrupted`，并把副作用不确定性保留在 Journal。按副作用类别
只有人工确认后的安全只读节点可以 Resume；`write`/`unknown` 继续要求人工处理。
Resume 或 amend 在取消前先校验 scope、冻结 definition hash 和 launch conflict；
UI 明确创建 successor，并切换到 successor 的 `run-started`，不能在原 run 上就地
并发。上述 successor 与冷接管边界属于 Runtime 合同；Source40 离线已覆盖，Native40
原生 scope 的整体汇总仍为 18 PASS、3 FAIL、2 NOTRUN。

取消令牌触发后，Driver 返回的取消相关错误必须以节点 `Cancelled` 结算，并最终
形成 `RunStatus::Cancelled`；普通 Driver 错误在取消触发前仍以 `Failed` 结算并保留
原错误。`parallel` 失败会取消在途兄弟，但首个真实的非取消 Driver 错误优先保留，
不会被兄弟的取消错误覆盖。

## RPC 业务结果与产物边界

保存定义的 `listSavedWorkflows`、`getSavedWorkflow`、`updateSavedWorkflowMeta`、
`deleteSavedWorkflow` 和 `moveSavedWorkflow` 使用源协议的业务结果联合：成功返回
`ok: true`，定义不存在、名称冲突、范围不匹配、参数非法或移动目标无效返回
`ok: false` 与有界 `reason`。授权存储、会话归属和协议解码失败仍作为 RPC 错误拒绝，
不能伪装成业务失败。`listSavedWorkflowRuns` 成功结果只包含 `runs` 和 `truncated`；
运行摘要不泄露未定义的扩展字段。

产物投影使用封闭的源协议 `kind` 集合（`file`、`markdown`、`chart`、`table`、
`metrics`、`board`）；未知 kind 直接拒绝。产物版本必须是 `1..=16` 的整数，每个
产物最多保留 16 个版本；版本记录必须包含 `version` 和非负整数 `publishedAt`，其余
标题、描述、类型、URI、路径和字节字段按源 schema 的长度及类型校验。宿主不会把未知
kind 默认为 `file`，也不会把越界或缺失版本默认为 `1`；这样可避免错误数据在 UI 中
看似有效并破坏冷恢复。

ArtifactStore 的 Markdown 读取必须保留完整正文、DTO 和真实文件引用，不能只返回
标题或摘要。Native41 Main 的 `report.json` 已确认 shadowDOM 正文含双 marker、7213
bytes 且无 frontend/protocol faults；剩余失败是正文 UI 定位断言，不改变 ArtifactStore
正文事实。Source40
的 `useTextSelection` 合成滚动同步修复已有 6 项行为测试和 TSC 证据，Native40
已绑定该来源，但整体 scope 仍按原生报告结果单独判定。

## 展示与验证

原工作流列表、参数表、运行时间线、详情、workspace transcript、节点结果及
产物页面继续使用原服务接口。定义展示由 TypeScript 改为 JSON。控制节点以
阶段、边和日志投影，不向原严格叶节点 schema 塞入未知节点类型。

Agent 节点的 workspace transcript 没有第二份工作流日志：父会话 Journal 的
`actor-started` 只提供 `actorSessionId`、`parentSessionId`、`runId`、`nodeId`、
`nodeAddress` 和 `cwd` 绑定，Host 还必须从 actor 自身 Resource Journal 的
`actor-bound` 事实与 Session 项目根核对后，读取该 actor 的 Transcript。Transcript
中的 Read、Glob、Grep 映射为 `world-read`，Bash、PowerShell、Edit、Write 映射为
`world-run`；节点正文和成功/失败状态来自对应的 `ToolResult`。未知工具、未绑定的
父会话/run、项目根不一致或缺少绑定字段均不投影，查询仍返回原有严格 workspace
节点形状。在线和冷恢复都走同一 Runtime 读取入口，不从文件日志或前端缓存拼接事实。

离线行为测试覆盖顺序、并行、条件、循环、预算、取消、Journal 先写失败、完整
循环地址、冷恢复、完成写操作复用和不确定副作用拒绝。原生真实模型验收另使用
`tooling/native-live/parallel-agents.workflow.json`；测试计划存在不代表实际运行
通过，运行证据与未验证范围记录在 `docs/frontend-acceptance-matrix.md`。

当前来源/验证边界：Source39 的 archive owner-root 修复 workspace 为 55 suites、
3087 passed、0 failed、8 ignored，尚未生成 Native39 binary；Source40 离线统一检查为
55 suites、3094 passed、0 failed、8 ignored，严格 Clippy 33.50s，DocTests、fmt、
前端 22 files/67 tests、scripts 55 pass/1 macOS skip、CSS、gates、build 和 native
harness 16/16 均通过。Source41 当前 workspace 为 55 suites、3095 passed、0 failed、
8 ignored，严格 Clippy 25.77s、DocTests 13 suites/1 passed/0 failed/0 ignored、
harness 16/16、fmt 和批量脚本回归通过。Native41 已生成 binary
`BF88C1FA7F0030DDA1239E71A83B515EA469C112AFF3DFC7F557A68F74803904`，大小
109416448 bytes；frontend/dist 仍是 Source40，Native41 provenance 已绑定，full batch 已完成但有 5 个失败 scope。Source40 两次 compile-fail 日志保留，readonly
`LaunchComparison` 的 `Copy`、test-crate 引用和仅 test-helper 的 cfg 已修复；
Native40 上一版原生 full batch 为 18 PASS、3 FAIL、2 NOTRUN，权威
`verified-summary.json` 仍待 controller 输出。Native41 full batch 已结束，为 18 PASS、
5 FAIL、0 NOTRUN；失败范围包括 Main 正文 UI 定位、Worktree archive draft 定位、
Controls folded Radix 定位、Session Reconnect partial/stop 窗口和 Error Retry/Hook
marker 行为。Workflow Controls 当前计划为 88 steps，Native41 已完成 1024 项并推进到
seq 5153。Source42 retained 入口的离线检查已完成，当前 CUA native disabled。Source42 当前离线检查已完成：前端 24 files/76 tests、scripts
65 pass/1 macOS skip、CSS/build、workspace 65 suites/3103 passed/0 failed/8 ignored（52
unit/integration suites 加 13 doc suites）、最终 fmt/diff、严格 Clippy 38.16s、DocTests、
harness 64 项/63 pass/1 skip 和 PowerShell wrapper 均通过。feature platform 第二轮 8 项
（含 `trusted_main_webview_identity`）和 feature Clippy 69s 已通过。mcpOAuth temporary
refresh 专项仅改测试内容 51 add/8 delete，CRLF 已归一化为 HEAD LF，归一化 preimage 与
HEAD 一致。save/PDF 使用现有依赖，benchmark 仅限隔离 exports 目录，生产 native picker
保持原生路径；OS picker/tray/notification/manual 边界仍待原生验证。Native42 binary
SHA 为 `6178F767D421BB53EF46C1A4147FFC19AA4CCC42D0687926C519673D65406D28`、大小
109607424 bytes，`out/native-live/source-provenance-native42.json` 已绑定当前 binary。
FullAcceptance wrapper 已扩展为 25 组并新增 Exit/PDF。权威
`out/native-live/native42-full-batch/batch-summary.json` 显示 19 组通过、6 组失败，且均绑定
上述 Native42 SHA。通过 scope 包含 Main 179、MCP/Skills/Memory 169、Automation 118、
Scheduled 53、Layout 22、Browser 99、Editor 24、Goal 76、Groups 101、Resources 155、
Selection/Rewind 66、Reconnect 50、Error Retry/Hook 101、Feedback 62、Attachment 40、
General 78、Model 82、Permission 54、Task Menu 57。失败 scope 为 Worktree GUI 62/75、
Workflow Controls 66/90、Desktop Controls 26/30、Hook Trust 20/61、Exit 31/50、PDF
23/25；Source44 正处理这些范围，尚未 build/native 验证。Native42
Main 的 `firstInteractiveMs=1623` 及资源字段仅是本轮隔离采样；layout artifact
`out/native-live/native42-full-batch/07-ui-layout-final/appearance-pixel-diff.json` 的
整 viewport changed ratio 为 `4.339846%`、RGB changed ratio 为 `1.787944%`，两张卡片区域
changed ratio 为 `3.505716%`、`2.574888%`，只限定外观页。当前配置事件记录含 3 个
unknown fields，未展开任何配置值；OS picker/tray/notification/manual 和 release 性能预算
仍不以这些隔离字段判定通过。
Native44 批次随后收口为 25 组、20 PASS、5 FAIL；其中 PDF 25/25 geometry 和 DOCX
22/22 为局部通过，Worktree GUI、Workflow Controls、Goal Subagents、Resources、Exit
仍失败，Source45 正处理后续 precheck 与复测，不能据此宣称整体交付。
Native41 历史批次仍为 23 范围。入口为 `tooling/native-live/native-sequential.ps1 -FullAcceptance`，其中
包含 `tooling/native-live/ui-layout-final-plan.json` 和可参数化的
`tooling/scripts/native-layout-pixel-diff.ps1`；这些是验收入口与比较方法，不是通过结论。

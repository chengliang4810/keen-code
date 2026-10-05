# ZCode 前端替换与 Rust 工作流验收矩阵

## 当前结论

2026-10-05 追加真实模型性能排查：Rust 接管有界工作区索引、匹配与排序，移除前端
搜索 Worker；高亮池按 File/Diff 生命周期挂载，最多两个 Worker，并修复流式内容 key
重复重建 Shadow DOM、无界 Shiki 缓存和重复 tokenize。最终原生性能 58/58、实际
Read/Write/Bash、冷恢复、连续对话、停止与 GC 补验 93/93，通过 42 次宿主 IPC 健康检查。
新会话空闲 RSS 667.19 MiB、整机 CPU 0.0356%；代码回复后的六个 30 秒窗 CPU
0.0202%–0.0580%、GC 后主页面堆约 37.45 MiB 稳定。原配置桌面恢复后 30 秒采样
609.79 MiB / 0.0258%，原 15 个会话及配置摘要一致，启动新增一个空会话。
前端 93 PASS、desktop 1152 PASS / 5 ignored、Notify 专项 2 PASS；严格 Clippy、格式、
类型/构建/CSS/设计/来源门禁通过。本轮未重跑下述完整历史功能，600 MiB 基础占用、
小时级泄漏及原生/GPU 内存归因仍待继续验证。详见
[`performance-e2e-2026-10-05.md`](performance-e2e-2026-10-05.md)。

此前 2026-10-04 至 10-05 修复空闲 CPU：统一整机口径、系统采样仅刷新内存、目录监听改为
纯 Rust 原生事件。Windows 原生面板关闭／打开／再关闭对照为 0.6899%→0.0508%、
1.1198%→0.0902%、0.6910%→0.0166%。资源取证步骤 40 PASS，独立通知／重订阅补验
29/29 PASS；长采样计划的后续 Git 断言因 30s 小于既有 60s 防抖而 FAIL，保留首轮报告。
Rust workspace 3141 PASS、严格 Clippy／格式、前端 90 tests 及脚本 76 PASS／1 skip。
内存完成约 11.8 分钟 RSS／私有提交曲线及两次脱敏 V8 堆，小时级与原生堆归因仍 pending。
最终收尾发现现有 Tao 0.35.3 键盘输入重入死锁，已固定官方修复源；旧构建双线程输入
探测复现卡死，新构建 2400 条消息通过。最终原生 42/42 与 6 次真实宿主 IPC 健康检查
通过，CPU 关闭／打开／再关闭为 0.0401%／0.1441%／0.0177%。最终 desktop 1150 PASS，
workspace 严格 Clippy／格式再次通过。真实中文 IME、RDP／休眠恢复仍 pending。
证据、版本与限制见 [`idle-resources-2026-10-05.md`](idle-resources-2026-10-05.md)。
当轮使用原配置恢复桌面，没有真实模型请求，没有重跑下述完整历史功能。

此前构建将左下角账户头像/用户名入口改成统一设置菜单，移除重复齿轮，
保留主题、语言、缩放与设置页独立返回箭头。Windows 原生 62/62、前端 90 tests、
类型/构建/CSS/设计/来源门禁通过，证据和 EXE SHA 见 `docs/sidebar-settings-menu-2026-10-04.md`。
已恢复同一手工桌面配置；本次没有模型请求，没有重新运行下述完整历史验收范围。

此前构建将左上角侧栏切换按钮固定为侧边栏图标，取消 Logo/hover 切换。
本次 Windows 原生 21/21、前端 90 tests、类型/构建/CSS/设计/来源门禁通过，
证据与 EXE SHA 见 `docs/sidebar-icon-2026-10-04.md`。以下标题、分支和完整功能结果
均保留为各自构建的历史证据，不视为本次全部重测。

随后修复 V4 自动会话命名触发，指定旧会话已通过真实模型生成“当前项目用途说明”。
当次标题原生回归 50/50、desktop 1141 tests、runtime 169 tests、相关严格 Clippy 通过，
证据与新 EXE SHA 见 `docs/automatic-title-2026-10-04.md`。手工改名保护与两次冷启动去重
已验证；下述分支入口 96 步、手工反馈 53 步、Native49 full25 保留为各自构建的历史证据。

随后修复新建对话的重复分支入口：原分支选择器保留，工作树改为独立名称/图标。
当次构建 worktree 原生 96/96 通过，证据与 SHA 见 `docs/branch-control-2026-10-04.md`；
下述手工反馈 53 步和 Native49 full25 分别保留为各自构建的历史证据。

2026-10-04 手工反馈追加修复：引导移除工作方向/模式选择、固定编程模式、模型供应商去分组标题，
并修复普通 conversation 的插件/命令路径校验及用户命令目录 scope。新构建原生回归
53/53、desktop 1138 tests、前端 90 tests 通过，详见 `docs/manual-feedback-2026-10-04.md`。
下文 Source49/Native49 的 full25 数量与 SHA 是此前冻结证据，不代表新构建重新运行全部 scope。

前端业务界面固定复制 ZCode 3.14.3 提交 `29628c9acdb81b703bbd4080c207a0e7ce5e276e`，
保留 DOM、CSS token、组件和 locale 结构；官方账号、支付、云端、外部消息、机器人、SSH、
WSL、Docker 与官方 ComputerUse 入口已裁剪，旧前端已移除（退役来源见 `docs/source-history.md`）。前端原有 `ChannelClient`
通过 TauriProtocol 接入 Rust 网关，保持 VQL 100–204；Journal 是权威事实源，`WorkflowDefinitionV1` 是 Rust serde 负责的纯
JSON 定义。`Source49` 是前一轮实现和证据构建标签。空工具配置的最小 UI 语义已修复，Agent settings、canonical
model scalar、统一 AgentState、catalog refresh 和 `injectAgentsMd` 已冻结。前端 28
files/90 Vitest、scripts 74 pass/1 macOS skip、typecheck/build/CSS 均通过；相关 Rust
desktop/tools/integrations 测试和 Clippy 也通过。Native49 resources scope 212/212 已
通过。Native49 Main 179/179 和 resources scope 212/212 已通过；第二轮 frozen full25
已完成 25/25 个 scope、2007 steps、failed=0。该结果覆盖矩阵列出的产品范围，OS 手工
picker/tray/notification/drag-and-drop 等边界仍不能据此宣称通过。

Source48 workspace 55 suites/3124 passed/0 failed/11 ignored 仍作为离线基线；Source49
相关范围为 desktop 1137 passed/8 ignored、tools 195 passed/1 ignored、integrations included，
strict related Clippy 43.20s。Native49 debug binary SHA 为
`DB6BFF1BC1FCDADD12A4F4B1B4012C8CAA59F9862EAAA5DB5EDA57F2D7DA9CB1`，110075392 bytes；
Source49 provenance 已绑定 Native49 UI/dist。

## Source49 实现矩阵

| 功能范围 | 当前实现事实 | 验收状态 |
| --- | --- | --- |
| Project/session/file | Main desktop 保持项目、会话、文件读写、对话流、停止、历史和冷恢复链路；Journal 是事实源 | Native49 scope 01 PASS |
| Git Worktree | 原生 Worktree GUI 覆盖创建、切换、交接、归档和移除生命周期 | Native49 scope 02 PASS |
| PTY/desktop tools | 桌面控制覆盖 PTY、终端和文件预览等保留工具入口 | Native49 scope 18 PASS |
| Browser/child WebView | 保留普通 Browser toolbar 与 managed child WebView；不等同 OS ComputerUse | Native49 scope 08 PASS |
| Providers/model | provider、model catalog、reasoning/effort 和设置回读使用 canonical scalar | Native49 scopes 19/21 PASS |
| Plugins/MCP/Skills/Memory | 本地 MCP、Skills、Memory、Plugins 的资源与删除/冷恢复链路 | Native49 scope 04 PASS |
| Automation | 本地 automation 与 scheduled automation 的创建、运行、失败和终态台账 | Native49 scopes 05/06 PASS |
| Workflow/JSON definition | workflow controls、actor、artifact、cancel/resume 和纯 Rust JSON 定义 | Native49 scope 03 PASS |
| Model settings/catalog | canonical scalar 为 `provider::model + effort`；writer/reader schema、stable ID 和候选刷新已统一 | Source49 frozen；Native49 full25 与相关 workspace/Clippy 已通过 |
| Agent state | `agents-state` 统一 disabled 状态和 builtin/plugin model override；runtime 与 UI 使用同一状态 | Source49 frozen；Native49 full25 与相关 workspace/Clippy 已通过 |
| `injectAgentsMd` | checkbox 状态贯穿 Runtime；global guard、冷恢复和 Source UI 原生回读已接入 | Source49 frozen；Native49 full25 与相关 workspace/Clippy 已通过 |
| Empty tools profile | UI 保存“自定义可用工具”空集合，重开回读零工具后恢复 Bash | Native49 resources 212/212 已通过 |
| User Agent spawn | 指定 child 与同 agent/turn/requestID 的 Bash 成功，父子 Turn 完成 | Native49 resources scope 已通过 |
| Cold recovery | 测试使用 `shutdown_session`；完整目录断言排除 `"reviewer":`，保留 `"code-reviewer":` | Native49 full25 已通过；相关测试通过 |
| Workflow/Journal/artifact | Rust 保持 Journal 权威、seq/resync、active barrier、artifact 和 workflow JSON 约束 | Native49 full25 已通过；独立 workflow 计数也已核对 |
| Resources/Agent spawn | Native49 resources 212/212：零工具回读、Bash 恢复、disabled/enabled、指定 UserAgent child 与父子 Turn 完成 | pass（resources 与 full25 scope 均通过） |

## 当前证据

| 范围 | 证据 | 状态 |
| --- | --- | --- |
| 2026-10-05 原生性能 | `out/native-live/performance-final/report.json`：58/58，27 次宿主 IPC；真实 DeepSeek Read 和代码回复、20002 文件、profile/堆/空闲曲线 | PASS（本轮性能范围，debug 样本不是峰值预算） |
| 2026-10-05 原生功能与 GC | `out/native-live/performance-functional-final/report.json`：93/93，15 次宿主 IPC；索引失效、预览更新、命令中心、真实 Read/Write/Bash、冷恢复、连续对话、停止后零活跃会话与 GC | PASS（本轮列出范围） |
| 2026-10-05 离线与版本 | `out/performance-delivery-ui-tests.log` 93 PASS；`out/performance-rust-tests.log` desktop 1152 PASS / 5 ignored；Notify 2 PASS；严格 Clippy/gates 与 source provenance 绑定见本轮报告 | PASS（当前相关范围） |
| 2026-10-05 原配置桌面 | `out/native-live/manual-performance-idle-20261005.json`、`manual-performance-data-preservation.json`：609.79 MiB / 0.0258%，原 15 个会话与配置 SHA 一致 | PASS（单次 30 秒空闲采样；native/GPU 长期分析 pending） |
| Source48 workspace baseline | `out/workspace-tests-native48-final-third.log`、`out/workspace-tests-native48-summary.json`：55 suites/3124 passed/0 failed/11 ignored | pass（历史离线基线） |
| Source49 frontend/scripts | `out/frontend-tests-native49-final.log`：28 files/90 tests；`out/frontend-build-native49-final.log` typecheck/Vite 通过；`out/frontend-css-native49-final.log` 通过；scripts 74 pass/1 macOS skip | pass（仅离线） |
| Source49 affected Rust | `out/affected-tests-native49-final.log`、summary JSON：desktop 1137 passed/8 ignored、tools 195 passed/1 ignored、integrations included；`out/affected-clippy-native49-final.log` strict related Clippy 43.20s | pass（相关范围，不是全 workspace 3125） |
| Native49 build/source | `out/native-build-native49.log`、`out/native-live/native49-build.json`、`out/native-live/source-provenance-native49.json`：SHA `DB6BFF1BC1FCDADD12A4F4B1B4012C8CAA59F9862EAAA5DB5EDA57F2D7DA9CB1`，110075392 bytes，27.06s | pass（仅构建与来源绑定） |
| Native49 Main | `out/native-live/native49-full-batch-second/01-zcode-desktop/report.json`：179/179；stream/file/cmd/stop/history、并行 actor、ArtifactStore 正文、cold no-replay、双 active cancel/终态均通过，无 frontend/protocol fault | pass（仅 Main scope） |
| Native49 resources | `out/native-live/native49-full-batch-second/12-local-resources-crud/report.json`：Resources 212/212；独立 `out/native-live/native49-resources-precheck/report.json` 同 scope 亦为 212/212 | pass（仅 resources scope） |
| Native49 second independent facts | `out/native-live/native49-second-workflow-independent.json`：Main 4 nodes/2 actors、cold records 17→17；Controls unique Read 0..1023、node reused 19、artifact/metadata 1069/1069 | pass（仅已完成 scope） |
| Native49 second resource summary | `out/native-live/native49-second-main-resource-summary.json`：firstInteractive 1290ms；idle CPU 0.0070829556%；working set 724873216/private 584732672 | pass（单次 debug 采样，不是 release 预算） |
| Native49 PDF inspection | `out/native-live/native49-full-batch-second/25-native-pdf-export/pypdf-independent-inspection.json`：1 页、960x540pt，标题/正文 marker 存在且无 KeenCode app chrome；Poppler preview 已复核 | pass（PDF scope 补充检查） |
| Native48 Resource200 | `out/native-live-native48-resources-precheck.log`：105/200 因实际菜单“自定义可用工具”与测试文案“自定义工具”不匹配失败；无 frontend/protocol fault，未发生模型调用 | 历史失败，保留原始证据 |
| Native49 frozen full25 | `out/native-live/native49-full-batch-second/batch-summary.json`：25/25 scope、2007 steps、failed=0；每个 scope 绑定 Native49 SHA | PASS（仅列出的产品范围） |

## Native49 FullAcceptance 范围表

第二轮使用同一 Native49 binary；`batch-summary.json` 记录 25/25 scope、2007 steps、
failed=0。所有路径以下按 summary 的 scope 目录列出。第一轮 23 PASS/2 FAIL 仅保留在
历史索引。

| # | 保留功能 scope | 第二轮证据 | 状态 |
| --- | --- | --- | --- |
| 01 | Main desktop | `out/native-live/native49-full-batch-second/01-zcode-desktop/report.json` | PASS |
| 02 | Worktree GUI | `out/native-live/native49-full-batch-second/02-native-worktree-gui/report.json` | PASS |
| 03 | Workflow Controls | `out/native-live/native49-full-batch-second/03-workflow-controls/report.json` | PASS |
| 04 | MCP/Skills/Memory | `out/native-live/native49-full-batch-second/04-local-mcp-skills-memory/report.json` | PASS |
| 05 | Local Automation | `out/native-live/native49-full-batch-second/05-local-automation/report.json` | PASS |
| 06 | Scheduled Automation | `out/native-live/native49-full-batch-second/06-scheduled-automation/report.json` | PASS |
| 07 | Layout/appearance | `out/native-live/native49-full-batch-second/07-ui-layout-final/report.json` | PASS |
| 08 | Browser child WebView | `out/native-live/native49-full-batch-second/08-browser-only-local-features/report.json` | PASS |
| 09 | Native Editor | `out/native-live/native49-full-batch-second/09-native-editor/report.json` | PASS |
| 10 | Goal Subagents | `out/native-live/native49-full-batch-second/10-goal-subagents/report.json` | PASS |
| 11 | Project Groups | `out/native-live/native49-full-batch-second/11-project-groups/report.json` | PASS |
| 12 | Resources CRUD | `out/native-live/native49-full-batch-second/12-local-resources-crud/report.json` | PASS |
| 13 | Selection/Rewind | `out/native-live/native49-full-batch-second/13-selection-side-rewind/report.json` | PASS |
| 14 | Session Reconnect | `out/native-live/native49-full-batch-second/14-session-reconnect/report.json` | PASS |
| 15 | Error Retry/Hook | `out/native-live/native49-full-batch-second/15-error-retry-hook/report.json` | PASS (101/101) |
| 16 | Assistant Feedback | `out/native-live/native49-full-batch-second/16-assistant-feedback/report.json` | PASS (62/62) |
| 17 | Attachment transfer | `out/native-live/native49-full-batch-second/17-attachment-send/report.json` | PASS (40/40) |
| 18 | Desktop Controls | `out/native-live/native49-full-batch-second/18-desktop-controls/report.json` | PASS (37/37) |
| 19 | General Settings | `out/native-live/native49-full-batch-second/19-general-settings/report.json` | PASS (78/78) |
| 20 | Hook Trust | `out/native-live/native49-full-batch-second/20-hook-trust/report.json` | PASS (61/61) |
| 21 | Model Settings | `out/native-live/native49-full-batch-second/21-model-settings/report.json` | PASS (82/82) |
| 22 | Permission Mode | `out/native-live/native49-full-batch-second/22-permission-mode/report.json` | PASS (54/54) |
| 23 | Task Menu/Unread | `out/native-live/native49-full-batch-second/23-task-menu-unread/report.json` | PASS (57/57) |
| 24 | Exit Confirmation | `out/native-live/native49-full-batch-second/24-exit-confirmation/report.json` | PASS (50/50) |
| 25 | PDF Export | `out/native-live/native49-full-batch-second/25-native-pdf-export/report.json` | PASS (25/25) |

## 保留功能与边界

| 范围 | 当前判断 | 证据边界 |
| --- | --- | --- |
| Browser toolbar / managed child WebView | 保留 | Native49 scope 08 PASS；文件名 `NativeBrowserView` 不代表 OS ComputerUse |
| Worktree GUI | 保留 | Native49 scope 02 PASS，覆盖原生 Worktree 生命周期 |
| Editor、PDF、DOCX | 保留 | Native49 scopes 09/25 PASS；DOCX 22/22 独立补充 report 也通过 |
| MCP、Skills、Memory、Plugins | 保留 | Native49 scope 04 PASS，覆盖本地资源链路 |
| OS picker、tray、notification、dragdrop、manual 操作 | 保留范围仍待原生证据 | 当前 CUA native disabled；不以浏览器或离线测试替代 |
| Remote attachment transfer | 仅裁剪 remoteWorkspace 可达路径 | 标准本地 Tauri 没有 `remoteSessionId`，不构成本地附件缺口 |
| 官方账号、支付、云端、外部消息、机器人、SSH、WSL、Docker、官方 ComputerUse 控制插件 | 明确裁剪 | 来源退役记录见 [`docs/source-history.md`](source-history.md) |

## 历史证据索引

历史失败原始 report/log 保留在 `out/`，这里只保留版本和链接，避免阶段结果污染当前
Source49 结论。

| 版本或范围 | 原始证据与结果 |
| --- | --- |
| Native44 FullAcceptance | `out/native-live/native44-full-batch/batch-summary.json`：25 组、20 PASS、5 FAIL；SHA `B04691C488F15663D82D0C9F363DA47A1AF9BBB72B0540E586114024A0FAC376` |
| Native44 corrected scopes | `native44-worktree-script-checked` 94/94、`native44-goal-corrected` 81/81、`native44-exit-corrected` 50/50；均为独立校正 scope；`native44-resources-final/report.json` 仍失败 |
| Native49 first full batch | `out/native-live/native49-full-batch/batch-summary.json`：23 PASS、2 FAIL；Resource 首次 Bash 裸 PowerShell 命令 exit 127 后模型修复，Exit 断言错误禁止合法 Read。原始失败不与第二轮合并 |
| Native49 corrected scopes | `out/native-live/native49-resources-shellfix/report.json`：212/212；`out/native-live/native49-exit-effectfix/report.json`：50/50；属于第一轮失败修正后的独立 scope |
| Native42 FullAcceptance | [`docs/native-acceptance-2026-10-03.md`](native-acceptance-2026-10-03.md)：19 PASS、6 FAIL，不覆盖 Source49 |
| Native43/44 PDF/DOCX | Native43 错误 PDF geometry 见 `native43-pdf-precheck/pypdf-independent-inspection.json`；Native44 PDF/DOCX 局部 scope report 仍在 `out/native-live/` |
| Native48 Resource200 | `out/native-live-native48-resources-precheck.log`：105/200 UI 定位失败，实际菜单为“自定义可用工具”，无 frontend/protocol fault，未发生模型调用 |
| Source43–48 | workspace/build/provenance 与离线前端日志仍在 `out/`，仅作为历史基线，不替代 Source49 resources/full25 |

## 来源与协议边界

ZCode 逐文件映射、SHA-256 和许可证见 [`docs/frontend-zcode-source.md`](frontend-zcode-source.md)、
[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)、
[`third-party/zcode/SOURCE-MAPPING.md`](../third-party/zcode/SOURCE-MAPPING.md) 和
[`third-party/zcode/LICENSE`](../third-party/zcode/LICENSE)。Rust 是 Journal、资源持久化、
RPC 和 Runtime 权威；前端是可丢弃投影。`ChannelClient` 保持 VQL 100–204，v4 wire 为 3，
snapshot 为 1；window-bound connection、seq gap/reset resync、active barrier 和纯 JSON
workflow 约束以协议文档为准。

原生测试使用隔离 provider/data root，不记录密钥、服务地址或 provider 私有值。用户
ZCode 配置误写/越界事件见 [`docs/zcode-baseline-config-incident.md`](zcode-baseline-config-incident.md)，
原始替换备份仍保留在 `D:/projects/keen-code-backups/zcode-replacement-20261002-215853`，
清单保留 6020 个文件及 patch；Source 树与用户 ZCode setting 当前哈希已复核未变，值不写入报告。
最终凭据扫描 `out/native-live/credential-scan-native49-postlive.json` 已通过：排除明确的
私有配置文件后，产品源码及本次 `out/` 产物的真实 API key、服务地址命中为零，读取错误
为零；既有 `output/` 历史私有日志单独统计并保持原状。未执行 Git commit 或 push。

每个 `pass` 必须附真实命令、隔离环境和 report/log/screenshot 路径；未覆盖的 OS 手工
项目保持 `pending`，不能把它们与 Native49 full25 的已覆盖 scope 混为整体产品结论。

## 2026-10-05 Git 交付快照复验

本节仅对应代码快照 `79a97a99e3241838d32072f04b3a70f9f0d72fb0`，后续交付提交
只增加 README 和文档。前文真实模型与原生交互结果是各自构建的历史证据，本次未重跑。
另一个聊天进行中的私有内存指标、WebView 内存策略和设置／侧栏延迟加载未纳入此快照。

环境：Windows x64、Node.js 24.13.0、pnpm 10.14.0、Rust/Cargo 1.98.1。
前端和供应链在独立目录
`C:/Users/chengliang/.codex/worktrees/git-delivery-validation/keen-code` 验证；
Rust 在相同 HEAD 的 `D:/projects/keen-code-validation-snapshot` 继续验证，
显式使用 `D:/projects/keen-code/target/native-desktop-tests`，不覆盖正在运行的桌面 EXE。
全部日志保留在原工作区 `out/git-delivery-20261005/`，不将生成日志纳入 Git。

| 命令／核验 | 本次结果 | 日志（上述目录下） |
| --- | --- | --- |
| `corepack pnpm@10.14.0 install --frozen-lockfile` | PASS，1030 packages | `snapshot-frontend-01-install.log` |
| `pnpm run typecheck` | PASS | `snapshot-frontend-02-typecheck.log` |
| `pnpm test` | PASS：Vitest 29 files／93 tests；脚本 76 PASS／1 macOS skip；设计与来源门禁 PASS | `snapshot-frontend-03-test.log` |
| `pnpm run lint:css` | PASS | `snapshot-frontend-04-lint-css.log` |
| `pnpm build` | PASS，7815 modules | `snapshot-frontend-05-build.log` |
| 前端验证后 `git diff --quiet`／`git diff --cached --quiet` | PASS，tracked clean | `snapshot-frontend-06-status.log` |
| `cargo fmt --all -- --check` | PASS | `snapshot-rust-fmt-d-drive.log` |
| `cargo test --workspace --all-targets --target-dir D:/projects/keen-code/target/native-desktop-tests -- --test-threads=1` | PASS：55 条 suite 结果／3143 PASS／8 ignored／0 failed，含测试子进程的结果行 | `snapshot-rust-test-all-targets-serial.log`、`snapshot-rust-test-summary.json` |
| `cargo test --workspace --doc --target-dir D:/projects/keen-code/target/native-desktop-tests` | PASS | `snapshot-rust-doc-final.log` |
| `cargo clippy --workspace --all-targets --target-dir D:/projects/keen-code/target/native-desktop-tests -- -D warnings` | PASS | `snapshot-rust-clippy-final.log` |
| `cargo build -p keencode-desktop --features native-desktop-tests --target-dir D:/projects/keen-code/target/native-desktop-tests` | PASS，仅构建验收程序 | `snapshot-rust-native-build-final.log` |
| Rust 验证后 `git diff --quiet`／`git diff --cached --quiet` | PASS，tracked clean | `snapshot-rust-status-final.log` |
| `cargo-deny --offline check advisories bans licenses sources`（0.20.2） | PASS，0 error／52 warnings | `snapshot-cargo-deny.log` |
| 五份上游许可证 SHA、1146 个 Material Icon SVG、Tao Apache-2.0 与固定 revision | PASS | `snapshot-provenance.log` |
| `git diff 9e4ebc9534917b48ffb2208cd847754dd0ad829e HEAD --check` | PASS | `snapshot-total-diff-check.log` |

首轮 Rust 因隔离树尚无 `apps/ui/dist` 退出，前端打包后重试又因 C 盘空间不足退出；
这两次失败原样保留于 `snapshot-rust-test-all-targets.log` 和
`snapshot-rust-test-all-targets-rerun.log`。随后复制已验证的 dist 到 D 盘同一代码快照，
并通过应用归档清理仅供本次验证的 C 盘工作树，最终结果按上表单独记录，不混合失败轮次。

D 盘并行全量测试曾在未改动的 MCP HTTP 集成测试出现 13 个超时失败，见
`snapshot-rust-test-all-targets-d-drive.log`。单独 MCP 单线程复测 23/23 通过，见
`snapshot-rust-mcp-serial.log`；随后完整 workspace 单线程复测通过，没有修改代码或
放宽超时。并行时限稳定性仍存在未定位因素，不将单线程通过描述成该因素已被修复。

既有提醒：pnpm 忽略 `msw` build script；Vite 有大 chunk 和 3 条 ineffective dynamic import；
cargo-deny 有 51 条重复依赖及 1 条未使用许可白名单警告。本次没有以关闭门禁消除提醒。
Windows MSVC 构建输出 import library 的 linker stdout 提醒；严格 Clippy 通过。
真实 provider、原生窗口交互、IME／RDP／休眠恢复及小时级内存泄漏保持本次 `pending`。

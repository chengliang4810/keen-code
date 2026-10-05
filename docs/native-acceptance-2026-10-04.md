# ZCode 前端替换与 Rust 工作流验收报告

2026-10-04 手工反馈的新构建验收见 `docs/manual-feedback-2026-10-04.md`：引导、模型分组标题和
普通对话资源页修复后，独立原生 53/53 通过。本文 Native49 full25 是修复前冻结证据，保留其原始 SHA 和范围。

## 当前结论

前端业务界面固定复制 ZCode 3.14.3 提交 `29628c9acdb81b703bbd4080c207a0e7ce5e276e`，
保留其 DOM、CSS token、组件和 locale 结构；官方账号、支付、云端、外部消息、机器人、
SSH、WSL、Docker 与官方 ComputerUse 入口已裁剪，旧前端已移除（退役来源见 `docs/source-history.md`）。前端原有
`ChannelClient` 通过 TauriProtocol 接入 Rust 网关，保持 VQL 100–204；Journal 是会话、消息、工具和 workflow 的权威事实源，
`WorkflowDefinitionV1` 由 Rust serde 负责纯 JSON 校验。`Source49` 是本轮实现和证据构建标签，
不是另一套前端来源。当前冻结的空工具配置最小 UI 语义、Agent settings、canonical model
scalar、统一状态、catalog refresh 和 `injectAgentsMd` 均已纳入验收。Source49 前端 28 files/90
Vitest、scripts 74 pass/1 macOS skip，typecheck/build/CSS 均通过；相关 Rust/desktop
测试也已通过。Native49 Main 179/179、resources scope 212/212 已通过。第二轮 frozen
full25 已完成 25/25 个 scope、2007 steps，原始 failed=0；这只覆盖列出的产品范围，OS
手工 picker/tray/notification/drag-and-drop 等边界仍不能据此宣称通过。Source48
的 3124 workspace 结果和 Resource200 失败仅保留为历史基线。

Native49 debug binary SHA 为
`DB6BFF1BC1FCDADD12A4F4B1B4012C8CAA59F9862EAAA5DB5EDA57F2D7DA9CB1`，110075392 bytes，
build 27.06s；Source49 provenance 已绑定 Native49 UI/dist。当前最终计划为 2007 steps
加 DOCX 22 steps。

本次在 Windows 原生 Tauri/WebView2（Chrome 148）中运行，所有 scope 的模型配置均为
OpenAI Chat Completions（`chat_completions`）+ `deepseek-v4.1-flash`，使用隔离私有配置。
Main 通过新前端发送真实流式请求、连续对话、文件读取/修改与命令执行，核对停止、历史
恢复以及两个真实模型 actor 的并行工作流；结果经过 Rust 网关、Runtime、Journal 再显示
在界面，未使用模拟模型响应。主流程视口为 1280×820、DPR 2；外观像素比较单独使用
1280×820、DPR 1，与固定来源截图对齐。

原生入口为 `tooling/scripts/native-live-e2e.mjs`（`pnpm run test:native-live`）；完整串行入口
为 `tooling/native-live/native-sequential.ps1` 的 `-FullAcceptance`。需先构建
`cargo build -p keencode-desktop --features native-desktop-tests`，再通过 `-ProviderConfig`
传入私有配置、`-AttemptDirectory` 指定新的证据目录；不得覆盖已冻结的验收目录。

## Source49 当前实现

| 范围 | 当前事实 | 状态 |
| --- | --- | --- |
| Model catalog | canonical scalar 为 `provider::model + effort`；修复 writer/reader 的字段契约和候选刷新 | Native49 full25 已通过；相关 workspace/Clippy/DocTests 也通过 |
| Agent state | 统一 `agents-state`、disabled 状态、builtin/plugin model override 和稳定 ID | Native49 full25 已通过；相关 workspace/Clippy/DocTests 也通过 |
| `injectAgentsMd` | checkbox 状态贯穿 Runtime；补 global guard、冷恢复和 Source UI 原生回读 | Native49 full25 已通过；相关 workspace/Clippy/DocTests 也通过 |
| Cold recovery | 新测试使用 `shutdown_session`；不把永久关闭的 `close_session` 当作可恢复重启 | Native49 full25 已通过；相关测试保持通过 |
| Agent directory assertion | 按完整 `"reviewer":` 排除目标项，同时保留 `"code-reviewer":` | Native49 full25 已通过；相关测试保持通过 |
| Remote attachment transfer | 仅裁剪 remoteWorkspace 可达路径；标准本地 Tauri 没有 `remoteSessionId` | 裁剪边界，不是本地附件缺口 |
| Empty tools profile | UI 显示并保存“自定义可用工具”空集合；重开后回读零工具，再恢复 Bash | Native49 resources 212/212 已通过 |
| User Agent spawn | 指定 UserAgent child，Bash 成功绑定同一 agent/turn/requestID，父子 Turn 均完成 | Native49 resources scope 已通过 |

## 当前证据

| 范围 | 实际证据 | 状态 |
| --- | --- | --- |
| Source48 workspace baseline | `out/workspace-tests-native48-final-third.log`、`out/workspace-tests-native48-summary.json`：55 suites/3124 passed/0 failed/11 ignored | pass（历史离线基线） |
| Source49 frontend/scripts | `out/frontend-tests-native49-final.log`：28 files/90 tests；`out/frontend-build-native49-final.log` typecheck/Vite 通过；`out/frontend-css-native49-final.log` 通过；scripts 74 pass/1 macOS skip | pass（仅离线） |
| Source49 affected Rust | `out/affected-tests-native49-final.log`、`out/affected-tests-native49-summary.json`：desktop 1137 passed/8 ignored，tools 195 passed/1 ignored，integrations included；`out/affected-clippy-native49-final.log` strict related Clippy 43.20s；fmt 通过 | pass（相关范围，不是全 workspace 3125） |
| Native49 build/source | `out/native-build-native49.log`、`out/native-live/native49-build.json`、`out/native-live/source-provenance-native49.json`：SHA `DB6BFF1BC1FCDADD12A4F4B1B4012C8CAA59F9862EAAA5DB5EDA57F2D7DA9CB1`，110075392 bytes，27.06s；Source49 UI/dist 已绑定 | pass（仅构建与来源绑定） |
| Native49 Main | `out/native-live/native49-full-batch-second/01-zcode-desktop/report.json`：179/179，stream/file/cmd/stop/history、并行 actor、ArtifactStore 正文、cold no-replay、双 active cancel/终态均通过，无 frontend/protocol fault | pass（仅 Main scope） |
| Native49 resources | `out/native-live/native49-full-batch-second/12-local-resources-crud/report.json`：Resources 212/212；独立 `out/native-live/native49-resources-precheck/report.json` 同 scope 亦为 212/212 | pass（仅 resources scope） |
| Native49 DOCX | `out/native-live/native49-docx-final/report.json`：22/22 | pass（独立 DOCX scope） |
| Native49 PDF inspection | `out/native-live/native49-full-batch-second/25-native-pdf-export/pypdf-independent-inspection.json`：1 页、960x540pt，标题/正文 marker 存在且无 KeenCode app chrome；Poppler preview 已复核 | pass（PDF scope 的补充检查） |
| Native49 layout pixel | `out/native-live/native49-full-batch-second/07-ui-layout-final/appearance-pixel-diff.json`：raw 10.610709%，对齐 dx=2/dy=-4 后 4.339846%，mean 1.787944% | pass（仅外观 scope，不能扩展为全产品像素等价） |
| Native48 Resource200 | `out/native-live-native48-resources-precheck.log`：第 105/200 步因 UI 菜单实际为“自定义可用工具”而计划使用“自定义工具”失败；无 frontend/protocol fault，尚未发生模型调用 | 历史失败，保留原始证据 |
| Native49 frozen full25 | `out/native-live/native49-full-batch-second/batch-summary.json`：25/25 scope、2007 steps、failed=0；每个 scope 均绑定 Native49 SHA | PASS（仅列出的产品范围） |

第二轮独立 workflow 证据 `out/native-live/native49-second-workflow-independent.json`
记录 Main 4 nodes/2 actors、left Read+Grep、6234-byte artifact/metadata markers、cold
records 17→17；Controls 的 1024 Read 地址 0..1023 全部成功，node reused 为 19（地址
0..18），旧 cancel 33 次成功 Read、successor 12 次成功 Read 且 predecessor 正确，
隔离 artifact/metadata 为 1069/1069。该证据与 full25 summary 的 25/25 结果相互独立，
用于核对关键 Journal/artifact 计数。

`out/native-live/native49-second-main-resource-summary.json` 的单次 debug 采样为
`firstInteractiveMs=1290`、idle CPU `0.0070829556%`、working set `724873216`、private
bytes `584732672`。这是第二轮单次 debug 采样，不沿用第一轮摘要指标，也不是 release
installer、50 MB 安装包或长期性能
预算结论；debug binary 的 110075392 bytes 也不代表安装包大小。最终凭据扫描报告
`out/native-live/credential-scan-native49-postlive.json` 已通过：排除明确的私有配置文件后，
产品源码与本次 `out/` 验收产物的真实 API key、服务地址命中均为零，读取错误为零。
既有 `output/` 历史私有日志单独统计、保持原状，不混入本次通过范围。

| 调试版采样状态 | 采样时长 | CPU（整机占比） | 工作集 bytes | private bytes |
| --- | --- | --- | --- | --- |
| 空闲 | 11030 ms | 0.007083% | 724873216 | 584732672 |
| 单个活跃会话 | 2138 ms | 6.065833% | 926552064 | 827379712 |
| 两个活跃工作流 actor | 4142 ms | 1.188285% | 913260544 | 909164544 |
| 两个 actor 取消前 | 2101 ms | 1.041171% | 884715520 | 953749504 |

三个活跃采样都确认了实际活跃状态；数值来自同一进程、不同执行时刻，并非可直接相减的
稳定性能基准。空闲工作集约 691 MiB，内存偏高，需要在 release 构建中复测和定位；
不能将本次 CPU、冷启动或 debug 二进制大小推广为正式安装包已达预算。

Native49 resources 的 212/212 只证明 resources scope 在 Native49 binary 上通过，不扩展
为 full25。Native48 Resource200 的 selector 失败明确是菜单文案不匹配；该 raw report
保留为历史证据，不再作为 Native49 当前失败根因。Source49 的必要回归范围是零工具
语义、canonical writer/reader、统一 AgentState、稳定 ID、候选刷新、disabled 状态、
model override、`injectAgentsMd` 和冷恢复。

## 最终验收门槛

| 门槛 | 必须有的证据 | 当前状态 |
| --- | --- | --- |
| Rust offline baseline | Source48 workspace 3124 passed；Source49 affected desktop/tools/integrations 相关测试和 Clippy 通过；无新全 workspace 3125 统计 | pass（离线范围） |
| Source49 binary | Native49 binary、source provenance 和 Source49 UI/dist 绑定证据已生成 | pass（仅构建与来源绑定） |
| Native49 full25 | `out/native-live/native49-full-batch-second/batch-summary.json`：25/25、2007 steps、failed=0；DOCX 22/22 为独立补充 scope | pass（仅列出的产品范围） |
| Resources regression | Native49 resources 212/212 已通过，覆盖零工具保存回读、Bash 恢复、disabled/enabled、UserAgent child 和父子 Turn | pass（仅 resources scope） |
| Worktree GUI / browser child WebView | Native49 scopes 02/08 已有真实原生 report | pass（覆盖范围内） |
| OS picker、tray、notification、drag-and-drop、manual 操作 | 尚无对应 OS 手工证据；不以浏览器或离线测试替代 | pending；CUA native 仍 disabled |

## Native49 FullAcceptance 范围表

第二轮使用同一 Native49 binary；`batch-summary.json` 记录 25/25 scope、2007 steps、
failed=0。所有路径以下按 summary 的 scope 目录列出；第一轮结果不与第二轮合并。

原始 `frontend-errors.json` 保留了 7 条 Tauri 异步回调在页面重载后找不到 callback id 的
警告：browser scope 6 条、session-reconnect scope 1 条。各 scope 的 `frontendFaults` 和
`protocolFaults` 均为空；这不等于所有原始诊断文本为空，也未通过删除警告制造通过。

独立审计见 `out/native-live/native49-second-full-independent-audit.json` 和
`out/native-live/native49-final-root-audit.json`：25 个原始计划与冻结预检哈希一致，
格式化计划、report、plan-snapshot、run-context 的哈希相互匹配；DOCX 22 步也通过同样核验。

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

## 历史验收证据

历史失败原始 report/log 保留在 `out/`，以下只保留索引，不把阶段结果混入当前结论。

| 版本或范围 | 原始证据与结果 |
| --- | --- |
| Native44 FullAcceptance | `out/native-live/native44-full-batch/batch-summary.json`：25 组、20 PASS、5 FAIL；绑定 SHA `B04691C488F15663D82D0C9F363DA47A1AF9BBB72B0540E586114024A0FAC376`。五个失败 scope 的原始 report 仍在该目录。 |
| Native44 corrected scopes | `native44-worktree-script-checked` 94/94、`native44-goal-corrected` 81/81、`native44-exit-corrected` 50/50 仅是独立校正 scope；`native44-resources-final/report.json` 仍 `passed=false`。 |
| Native49 first full batch | `out/native-live/native49-full-batch/batch-summary.json`：23 PASS、2 FAIL；Resource 首次 Bash 裸 PowerShell 命令 exit 127 后模型修复，Exit 断言错误禁止合法 Read。原始失败不与第二轮合并。 |
| Native49 corrected scopes | `out/native-live/native49-resources-shellfix/report.json`：212/212；`out/native-live/native49-exit-effectfix/report.json`：50/50；两者是第一轮失败修正后的独立 scope。 |
| Native44 PDF/DOCX | `out/native-live/native44-pdf-precheck/report.json` 25/25，独立 geometry 960x540；`out/native-live/native44-docx-final/report.json` 22/22。两者均为局部 scope。 |
| Native43 PDF | `out/native-live/native43-pdf-precheck/pypdf-independent-inspection.json` 保留错误的 792x612 Letter geometry 证据，不能当 final pass。 |
| Native42 | 详细 19 PASS/6 FAIL 记录见 [`docs/native-acceptance-2026-10-03.md`](native-acceptance-2026-10-03.md)，不覆盖 Source49 状态。 |
| Native48 Resource200 | `out/native-live-native48-resources-precheck.log`：105/200 UI 定位失败，实际菜单“自定义可用工具”与计划文案不一致；无 frontend/protocol fault，未发生模型调用。 |
| Source43–48 | 离线 workspace/build/provenance 记录仍在 `out/`；它们是历史基线，不替代 Source49 resources/full25。 |

## 产品边界与来源

保留普通 Browser toolbar、managed child WebView、Worktree GUI、纯 Rust
`WorkflowDefinitionV1`、Journal/run/artifact/cold recovery、MCP/Skills/Memory/Plugins、
Editor、Settings、主题和 locale。官方账号、支付、云端、外部消息、机器人、SSH、WSL、
Docker 和官方 ComputerUse 控制插件属于明确裁剪范围；来源退役词面见
[`docs/source-history.md`](source-history.md)。`NativeBrowserView` 等文件名不代表 OS
ComputerUse。

ZCode 映射、SHA-256 和许可证见 [`docs/frontend-zcode-source.md`](frontend-zcode-source.md)、
[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)、
[`third-party/zcode/SOURCE-MAPPING.md`](../third-party/zcode/SOURCE-MAPPING.md) 和
[`third-party/zcode/LICENSE`](../third-party/zcode/LICENSE)。Rust 持有 Journal、资源、
RPC 和 Runtime 权威状态；前端是可丢弃投影。`ChannelClient` 保持 VQL 100–204，v4 wire
为 3，snapshot 为 1；window-bound connection、seq resync、active barrier 和纯 JSON
workflow 约束以协议文档为准。

原生测试使用隔离 provider/data root，不记录密钥、服务地址或 provider 私有值。用户
ZCode 配置误写/越界事件见 [`docs/zcode-baseline-config-incident.md`](zcode-baseline-config-incident.md)，
原始替换备份仍保留在 `D:/projects/keen-code-backups/zcode-replacement-20261002-215853`，
清单保留 6020 个文件及 patch；Source 树与用户 ZCode setting 当前哈希已复核未变，值不写入报告。
未执行 Git commit 或 push。

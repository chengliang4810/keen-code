# ZCode 替换原生验收记录

## 当前交付状态

截至 2026-10-04，Native41 已完成 product build：SHA-256
`BF88C1FA7F0030DDA1239E71A83B515EA469C112AFF3DFC7F557A68F74803904`，debug
binary 109416448 bytes，构建记录为 `out/native-live/native41-build.json`，build
耗时 54.81s。Source41 workspace、Clippy、DocTests、fmt、harness 和批量脚本回归
已通过；`out/native-live/source-provenance-native41.json` 已完成绑定，frontend
dist/runtime 与 Source40 一致。Native41 full batch 已结束：18 个 scope 通过、5 个
失败、0 个 NOTRUN，整体交付仍 pending。

Source42 当前离线检查已完成：前端完整测试 24 files/76 tests、scripts 65 pass/1 macOS
skip、CSS/build；最终 workspace 为 65 suites/3103 passed/0 failed/8 ignored（52
unit/integration suites 加 13 doc suites）；严格 Clippy 38.16s、fmt/diff、DocTests、
feature platform 第二轮 8 项、feature Clippy 69s、harness 64 项/63 pass/1 skip 和
PowerShell wrapper 均通过。mcpOAuth temporary refresh 专项通过，仅测试内容 51 add/8
delete，并将 CRLF 归一化为 HEAD 的 LF；归一化 preimage 与 HEAD 一致。Native42 binary
与 `out/native-live/source-provenance-native42.json` 已绑定当前 binary。权威
`out/native-live/native42-full-batch/batch-summary.json` 已收口为 25 组、19 组通过、
6 组失败；整体验收仍 pending。Source43 已有离线 workspace/build/provenance 证据，但全
25 组尚未复验；Source44 前端与 Native44 build/离线测试已通过，Native44 FullAcceptance
已收口为 20 PASS、5 FAIL。当前 checkpoint 详见
[`docs/native-acceptance-2026-10-04.md`](native-acceptance-2026-10-04.md)。

Native40 的原始 `out/native-live/native40-full-batch/batch-summary.json` 受 wrapper
`VoidTaskResult` 影响，把多个实际完成的 scope 写成 `passed=false`；Native40 的
18/3/2 只作为上一 binary 的最后完整原生基线。Native41 的权威批次文件为
`out/native-live/native41-full-batch/batch-summary.json`，每个 scope 的 scalar exit
code 已正确记录。

## Native41 当前 full-batch 结果

| Scope | 实际结果 | 判断 |
| --- | --- | --- |
| `01-zcode-desktop` | `report.json`：第 117 项失败；shadowDOM 正文真实包含双 marker、7213 bytes，`frontendFaults=[]`、`protocolFaults=[]`；正文内容本身已核实 | fail（单一 UI 定位） |
| `02-native-worktree-gui` | `report.json`：第 51 项 handoff 通过；第 55 项 archive receipt removed、target absent、git removed 后，UI draft 隐藏 workspace-path 定位失败 | fail（后续修复） |
| `03-workflow-controls` | `report.json`：第 54 项已通过，1024 项成功；第 64 项 folded Radix modelSubmenu hiddenPortal 被错误判定为 visible | fail（单一 UI 定位） |
| `04-local-mcp-skills-memory`、`05-local-automation`、`06-scheduled-automation`、`07-ui-layout-final`、`08-browser-only-local-features`、`09-native-editor` | 各自报告实际完成；插件卸载 `removeCache=false` 成功，layout 另见下方像素证据 | pass（局部范围） |
| `10-goal-subagents`、`11-project-groups`、`12-local-resources-crud`、`13-selection-side-rewind`、`16-assistant-feedback`、`17-attachment-send`、`18-desktop-controls`、`19-general-settings`、`20-hook-trust`、`21-model-settings`、`22-permission-mode`、`23-task-menu-unread` | 各自报告实际完成 | pass（局部范围） |
| `14-session-reconnect` | 第 25 项失败；真实 provider 整条回复一次提交结束，没有出现 page reload 后的 partial+stop 窗口 | fail（原生边界待修） |
| `15-error-retry-hook` | 第 22 项失败；真实 provider 200、多次 readonly tool 成功，但 reasoning 未满足 prompt 的 180s marker；unreviewed hook 保持 marker file untouched，timeout cleanup cancel 已发生 | fail（原生行为待修） |

所有 scope 目录位于 `out/native-live/native41-full-batch/`。上表共有 18 个
通过 scope 和 5 个失败 scope；局部通过不能推导整个应用通过。Source42 的 retained
入口尚未进入 Native41 binary。

## Native42 FullAcceptance 最终批次结果

| Scope | 当前实际结果 | 判断 |
| --- | --- | --- |
| 19 个通过 scope | `batch-summary.json`：01 Main 179、04 MCP/Skills/Memory 169、05 Automation 118、06 Scheduled 53、07 Layout 22、08 Browser 99、09 Editor 24、10 Goal 76、11 Groups 101、12 Resources 155、13 Selection/Rewind 66、14 Reconnect 50、15 Error Retry/Hook 101、16 Feedback 62、17 Attachment 40、19 General 78、21 Model 82、22 Permission 54、23 Task Menu 57 均为完整通过 | pass（仅 scope） |
| `02-native-worktree-gui` GUI | 62/75；删除的 closed-session tombstone 被错误拦挡。Source43 待修，尚未 build/native 验证 | fail |
| `03-workflow-controls` | 66/90；source provider closed 后仍命中 force-mounted submenu selector。此前 first 1024 与 cold resume 已通过，不改变本 scope 失败结论 | fail |
| `18-desktop-controls` | 26/30；HelpMenu 的 export 入口仍受 Onboarding 未完成影响 | fail |
| `20-hook-trust` | 20/61；真实模型返回纯文本 fixture，未提供 marker，也没有 tool | fail |
| `24-exit-confirmation` | 31/50；预期的 pendingWrite 未发生，权限链路仍在调查 | fail |
| `25-native-pdf-export` | 23/25；真实单页输出成 2 页，第一页为 app、第二页为 slide，dynamic print CSS/CSP 待查 | fail |
| Native42 FullAcceptance | 权威 `out/native-live/native42-full-batch/batch-summary.json`：25 组、19 PASS、6 FAIL，绑定 SHA `6178F767D421BB53EF46C1A4147FFC19AA4CCC42D0687926C519673D65406D28` | pending（不得汇总为整体通过） |

Native42 Main 本轮 `measurements` 记录 `firstInteractiveMs=1623`，高于 1.5s
目标；cold-start 隔离采样的 idle CPU 为 `0.014112%`、working set
`722874368`、private bytes `582856704`；one-active 的 CPU 为 `6.140988%`、working
set `839659520`、private bytes `733634560`；two-actors 的 working set 为
`939540480`、private bytes `963743744`、CPU `1.022232%`，且
`concurrentActorsObserved=true`。这些是本轮
隔离 debug/WebView2 运行的报告字段，不是 release 性能预算或长期基准。

`out/native-live/native42-full-batch/07-ui-layout-final/appearance-pixel-diff.json`
已绑定 Native42 binary：两张卡片的几何和标题一致，整 viewport 对齐后的 changed
ratio 为 `4.339846%`，RGB changed ratio 为 `1.787944%`；两张卡片区域 changed
ratio 分别为 `3.505716%`、`2.574888%`。该结果只限定外观页和本次对齐方法，不代表
全窗口或全产品像素等价。

## Source40/41 离线与 Native40/41 构建

| 项目 | 证据 | 状态 |
| --- | --- | --- |
| Rust workspace | `out/native-live/workspace-tests-native40-summary.json`、`out/workspace-tests-native40-final.log`：55 suites、3094 passed、0 failed、8 ignored | pass（离线） |
| Clippy/DocTests/fmt | `out/workspace-clippy-native40-final.log` 严格 Clippy 33.50s；`out/workspace-doctests-native40-final.log`、`out/fmt-native40-final.log` 通过 | pass（离线） |
| Frontend/harness | 前端 22 files/67 tests，scripts 55 pass/1 macOS skip，CSS/gates/build 通过；`out/native-harness-tests-native40-final.log` 16/16 | pass（离线） |
| Source41 当前 workspace | `out/workspace-tests-native41-final.log`、`out/native-live/workspace-tests-native41-summary.json`：55 suites、3095 passed、0 failed、8 ignored；严格 Clippy 25.77s、DocTests 13 suites/1 passed/0 failed/0 ignored、`out/native-harness-tests-native41-final.log` 16/16、fmt 和批量脚本回归通过；provenance 已绑定 | pass（离线） |
| Source42 当前离线 | `out/frontend-tests-native42-complete.log`：24 files/76 tests、scripts 65 pass/1 macOS skip；`out/frontend-css-native42-first.log`、`out/frontend-build-native42-final.log` 通过；`out/workspace-tests-native42-second.log`：52 unit/integration suites/3102 passed/0 failed/8 ignored，加 13 doc suites/1 passed，合计 65 suites/3103 passed/0 failed/8 ignored；`out/workspace-clippy-native42-final.log` 严格 Clippy 38.16s、`out/fmt-native42-normalized.log`、diffcheck 和 `out/workspace-doctests-native42-final.log` 通过；mcpOAuth temporary refresh 专项通过，仅测试内容 51 add/8 delete，CRLF 已归一化为 HEAD LF，归一化 preimage 与 HEAD 一致；`out/native-platform-feature-tests-native42-second.log` 第二轮 8 项通过，含 `trusted_main_webview_identity`；`out/native-feature-clippy-native42-final.log` feature Clippy 69s、`out/native-harness-tests-native42-final.log` 64 项/63 pass/1 skip、`out/native-sequential-tests-native42-final.log` PowerShell wrapper 通过；`out/native-live/source-provenance-native42.json` 已绑定当前 binary | pass（离线范围；原生批次 19 PASS、6 FAIL） |
| Compile history | `out/workspace-tests-native40-initial-compile-fail.log`、`out/workspace-tests-native40-second-compile-fail.log` 保留两次失败；readonly `LaunchComparison` 的 `Copy`、test-crate 引用和仅 test-helper 的 cfg 已修复 | 当前最终离线结果通过，历史失败保留 |
| Native40 binary/source | `out/native-live/native40-build.json`、`out/native-live/source-provenance-native40.json`；SHA `418F80...0F89`，109412864 bytes | pass（仅构建与来源绑定） |
| Native41 binary/source | `out/native-live/native41-build.json`、`out/native-live/source-provenance-native41.json`；SHA `BF88C1FA7F0030DDA1239E71A83B515EA469C112AFF3DFC7F557A68F74803904`，109416448 bytes，build 54.81s；frontendBuild 仍指向 Source40 final build | pass（构建、来源绑定；full batch 有 5 个失败 scope） |
| Native42 binary | `out/native-live/native42-build.json`、`out/native-live/source-provenance-native42.json`；SHA `6178F767D421BB53EF46C1A4147FFC19AA4CCC42D0687926C519673D65406D28`，109607424 bytes，`native-desktop-tests` build 已退出 0；workspace 65 suites/3103 passed/0 failed/8 ignored，feature platform 第二轮 8 项、feature Clippy 69s、fmt/diff 通过；FullAcceptance 25 组为 19 PASS、6 FAIL | pass（仅构建、来源绑定与离线证据，整体验收未完成） |

## Native40 基线资源与视觉证据

Native40 `01-zcode-desktop/report.json` 的 `measurements` 是上一版隔离
debug/WebView2 采样：`firstInteractiveMs=1565`；idle-native 为 CPU
`0.01415051621083137%`、working set `724361216`、private bytes `587403264`；
one-active-session 为 CPU `8.288738948348069%`、working set `856997888`、
private bytes `746917888`；two-active-workflow-actors 为 CPU
`0.9781774155445311%`、working set `987828224`、private bytes `1016041472`，
`concurrentActorsObserved=true`。这些值不是 release 性能预算或安装包大小。

Native40 `07-ui-layout-final/appearance-pixel-diff.json` 对 Source commit
`29628c9acdb81b703bbd4080c207a0e7ce5e276e` 与 Native40 做 1280×820、DPR1 比较：
标题和卡片尺寸一致，卡片对齐偏移 `dx=2`、`dy=-3.9765625`；whole aligned
changed ratio 为 4.339846%，两张卡片区域为 3.505716% 和 2.574888%。Source
的 `window-maximized` class、HeadlessChrome154 与原生 Chrome148、品牌裁剪和窗口
chrome 属于有意差异；该证据不宣称全页或全产品像素相等。

## Native41 当前验收边界

| 待处理范围 | 已确认事实与下一步 |
| --- | --- |
| Native41 full batch | 23 plans 已结束，18 PASS、5 FAIL、0 NOTRUN；Native41 不是整体通过。 |
| Main 正文 | shadowDOM 正文双 marker/7213 bytes、无 frontend/protocol faults 已确认；只需修第 117 项 UI 定位断言。 |
| Worktree GUI | 保留第 51 项 handoff；修复第 55 项 archive receipt 后 UI draft 隐藏 workspace-path 定位问题。 |
| Workflow Controls | 保留第 54 项、1024 项和 seq 5153；修正第 64 项 folded Radix modelSubmenu hiddenPortal 断言。 |
| Session Reconnect | 针对第 25 项 provider 一次提交结束、page reload 后无 partial+stop 的边界补测。 |
| Error Retry/Hook | 保留真实 provider 200、readonly tools 成功、marker 未满足和 timeout cleanup cancel 事实；修正后重跑第 22 项。 |
| Source42 离线与 retained 入口 | 严格 Clippy 38.16s、DocTests、feature Clippy 69s、feature platform 第二轮 8 项、harness 64 项/63 pass/1 skip、PowerShell wrapper、fmt/diff 和 workspace 65 suites/3103 passed/0 failed/8 ignored 已通过；Native42 FullAcceptance 已收口为 19 PASS、6 FAIL。Source43 已有离线/build 证据但全 25 组未复验；Native44 已完成 build、离线测试及 PDF/DOCX 局部 scope，FullAcceptance 已收口为 20 PASS、5 FAIL；OS picker/tray/notification/manual 边界仍待证据，当前 CUA native disabled。 |
| Frontend provenance | Native41 raw/composite/docs provenance 已绑定；Native42 raw/composite provenance 已 freeze 并绑定当前 binary，路径为 `out/native-live/source-provenance-native42.json`。 |

## 产品范围与安全边界

保留普通 Browser toolbar、managed child WebView、Worktree GUI、纯 Rust
`WorkflowDefinitionV1`、Journal、run/node/artifact、冷恢复、MCP/Skills/Memory/
Plugins、Editor、Settings、主题和 locale。普通浏览器 child WebView 不属于
ComputerUse。官方账号、支付、云端、外部消息、机器人、SSH、WSL、Docker 和
官方 ComputerUse 控制插件属于明确裁剪范围；来源历史见
[`docs/source-history.md`](source-history.md)。

Source44 正在由 frontend/controller/engine 处理 Native42 失败范围并保留 preimage；
OS picker、tray、notification、manual 操作的原生边界已单独列出，当前 CUA native
disabled，不能用 CUA 结果替代这些验收。

ZCode 来源固定为 3.14.3 commit `29628c9acdb81b703bbd4080c207a0e7ce5e276e`；
映射、SHA-256 和许可证见 [`docs/frontend-zcode-source.md`](frontend-zcode-source.md)、
[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)、
[`third-party/zcode/SOURCE-MAPPING.md`](../third-party/zcode/SOURCE-MAPPING.md) 和
[`third-party/zcode/LICENSE`](../third-party/zcode/LICENSE)。`ChannelClient` 继续
使用 VQL 100–204 的 `open/send/close`，v4 wire 为 3，snapshot 为 1；Journal、
active barrier、window-bound connection、seq resync 和 JSON workflow 约束仍以
协议文档为准。

原生测试使用隔离 provider/data root，不记录密钥、服务地址或 provider 私有值。
用户 ZCode 配置越界事件见 [`docs/zcode-baseline-config-incident.md`](zcode-baseline-config-incident.md)。该记录包含 3 个
unknown fields；本报告不展开配置值、密钥或服务地址。
原始替换备份仍保留在 `D:/projects/keen-code-backups/zcode-replacement-20261002-215853`。
未执行 Git commit 或 push。

## 历史版本表

| 版本 | 精简历史 | 当前关系 |
| --- | --- | --- |
| Native30–38 | 旧 binary 与局部 native-live 报告保留在 `out/` | 不覆盖 Native41 |
| Source39 | workspace 55/3087/0/8，archive owner-root 修复在源中 | Native40 前一代离线来源 |
| Source40 | workspace 55/3094/0/8、前端 22/67、harness 16/16 及全部离线门禁通过 | 当前离线基线 |
| Source41 | workspace 55/3095/0/8、Clippy 25.77s、DocTests/fmt、harness 16/16 和批量脚本回归通过；frontend/dist/runtime 与 Source40 一致，provenance 已绑定 | Native41 离线基线 |
| Native40 | SHA `418F80...0F89`，18 PASS、3 FAIL、2 NOTRUN；Main/GUI/Controls 的失败原因见上一版基线表 | 上一原生基线，保留复测依据 |
| Native41 | SHA `BF88C1FA...3904`，109416448 bytes；18 PASS、5 FAIL、0 NOTRUN | 当前原生基线，整体未交付 |
| Native42 | SHA `6178F767...D28`，109607424 bytes；binary build 已退出 0，workspace 65/3103/0/8、feature platform 第二轮 8 项、feature Clippy 69s、fmt/diff 通过；provenance 已绑定，25 组 FullAcceptance 为 19 PASS、6 FAIL | 当前构建候选，整体未交付 |
| Source42 | 当前离线完成：前端 24/76、scripts 65/1 skip、最终 fmt/CSS/build、DocTests、strict Clippy 38.16s、feature Clippy 69s、feature platform 第二轮 8 项、harness 64 项/63 pass/1 skip、PowerShell wrapper、workspace 65/3103/0/8 均有证据；mcpOAuth temporary refresh 专项仅改测试内容并通过，Native42 binary 与 provenance 已绑定；Source43 已有离线/build 证据但全 25 组未复验，Native44 已 build 且 PDF/DOCX scope 局部通过，FullAcceptance 已收口为 20 PASS、5 FAIL，当前 CUA native disabled | 当前 Source42 离线完成，Native42/44 整体验收未完成 |

失败、NOTRUN 或仅局部 scope 通过的证据不得汇总为产品整体通过。

# Ely/GPUI 原生 Windows 验收矩阵

## 最新验证记录：release44（2026-10-06）

当前源码对应 binary SHA-256 `c00ca8ec4fa4d399865d0b0d8d85c6c55f41e8d415134bb975663776ef0f949e`。55 个测试组共 2671 passed、0 failed、7 ignored；fmt、vendor fmt、Clippy、doctests、release/runner build 通过。来源清单含 1551 个输入，vendor 无漂移；原生进程采样 100 个模块，未命中 WebView/Electron/Node/V8/QuickJS。证据见 `out/ely-release-gates-44.log`、`out/ely-doctests-44.log`、`out/ely-source-inputs-44.json`、`out/ely-native-runtime-modules-44.json`。

真实模型 `unsupported-hooks-diagnostics-44` 45 actions 通过，进程自然退出；`resources-44` 在 ordinal 142 点击“记忆”时 UIA 子树不可用并超时，143 actions 后失败，进程自然退出。优先批次随后停止，release44 其余原生计划、视觉比较和性能尚未完成，不能宣称完整产品或像素一致性通过。证据见 `out/ely-native-batch-priority-44.log` 和相应 `native-run/report.json`。

## release42 离线门禁与原生批次结果（2026-10-06）

release42 目标 binary SHA-256 为
`aab17911004dfc2a2204d2f54bb92a61318682fb34d309196c143c1af4f079ee`，记录见
`out/ely-release42-sha256.txt`。`target/release/keencode-desktop.exe` 大小为
`40,609,280` bytes。workspace tests 为 55 groups、`2665 passed; 0 failed; 7 ignored`；
doctests、workspace/vendor fmt、fmt-check、Clippy、release build、runner build 和 Windows
production tree 均完成，source manifest 为 1550 个输入且未发现 vendor drift。release build
保留一个 `linker_messages` warning，不改变构建完成事实。

| 范围 | 证据 | release42 判断 |
| --- | --- | --- |
| workspace tests | `out/ely-workspace-tests-42.log` | **passed**；55 groups，`2665 passed; 0 failed; 7 ignored` |
| doctests | `out/ely-doctests-42.log` | **passed** |
| fmt、vendor fmt-check、workspace fmt-check、Clippy | `out/ely-workspace-fmt-42.log`、`out/ely-vendor-fmt-check-42.log`、`out/ely-workspace-fmt-check-42.log`、`out/ely-workspace-clippy-42.log` | **passed** |
| release/runner build 与生产依赖树 | `out/ely-release-build-42.log`、`out/ely-runner-build-42.log`、`out/ely-windows-production-tree-42.log` | **passed**；release binary 为 40,609,280 bytes |
| source manifest | `out/ely-source-inputs-42.json`、`out/ely-source-inputs-42.log` | **passed**；1550 inputs，无 vendor drift |
| cargo-deny | `out/ely-cargo-deny-42.log`、`out/ely-cargo-deny-42b.log` | 初次 `cargo deny` 因 `no such command` 失败；改用 `out/tools/cargo-deny-0.20.2-x86_64-pc-windows-msvc/cargo-deny.exe` 的 42b 审计通过，保留 `bans 0 errors / 66 warnings`、`advisories 0 errors / 12 notes` 的解释性记录 |

上述离线结果不等于完整产品通过；原生批次、idle 性能、像素比较和仍在修复的运行时范围按下表及后文各自状态记录。

### release42 原生计划逐项确认

下表中的报告均绑定 release42 SHA。除明确标为 `failed` 或 `partial` 的项目外，报告均为
被测进程自然退出、`exit_code=0`；runner 批次日志分别见
`out/ely-native-batch-functional-42.log`、`out/ely-native-batch-functional-B-42.log`、
`out/ely-native-batch-regression-42.log`、`out/ely-native-batch-stress-42.log` 和
`out/ely-native-batch-visual-42.log`。

| 计划 | 报告 | release42 结果 |
| --- | --- | --- |
| full42 | `out/ely-native/full-42/native-run/report.json` | **passed**；63 actions |
| cold-resume42 | `out/ely-native/cold-resume-42/native-run/report.json` | **passed**；35 actions |
| stop-reopen42 | `out/ely-native/stop-reopen-42/native-run/report.json` | **passed**；53 actions |
| goal-workflow42 | `out/ely-native/goal-workflow-42/native-run/report.json` | **passed**；68 actions |
| goal-lifecycle42 | `out/ely-native/goal-lifecycle-42/native-run/report.json` | **passed**；108 actions |
| automation42 | `out/ely-native/automation-42/native-run/report.json` | **passed**；75 actions |
| resources42 | `out/ely-native/resources-42/native-run/report.json` | **failed**；ordinal `259` 在点击返回工作区后等待 UIA 标签“任务输入”超时；报告 actions 为 260，runner `exit_code=1`，被测进程自然退出 |
| navigation42 | `out/ely-native/navigation-42/native-run/report.json` | **passed**；108 actions |
| hooks42 | `out/ely-native/hooks-42/native-run/report.json` | **passed**；123 actions |
| hooks-cold-resume42 | `out/ely-native/hooks-cold-resume-42/native-run/report.json` | **passed**；37 actions |
| settings42 | `out/ely-native/settings-42/native-run/report.json` | **passed**；102 actions |
| settings-light42 | `out/ely-native/settings-light-42/native-run/report.json` | **passed**；102 actions |
| provider-settings42 | `out/ely-native/provider-settings-42/native-run/report.json` | **passed**；119 actions |
| general-preferences42 | `out/ely-native/general-preferences-42/native-run/report.json` | **passed**；171 actions |
| font-settings42 | `out/ely-native/font-settings-42/native-run/report.json` | **passed**；72 actions |
| code-settings42 | `out/ely-native/code-settings-42/native-run/report.json` | **passed**；71 actions |
| code-settings-cold-resume42 | `out/ely-native/code-settings-cold-resume-42/native-run/report.json` | **passed**；26 actions |
| keybindings42 | `out/ely-native/keybindings-42/native-run/report.json` | **passed**；61 actions |
| keybindings-cold-resume42 | `out/ely-native/keybindings-cold-resume-42/native-run/report.json` | **passed**；35 actions |
| provider-model42 | `out/ely-native/provider-model-42/native-run/report.json` | **passed**；76 actions |
| workbench42 | `out/ely-native/workbench-42/native-run/report.json` | **passed**；140 actions，包含当前 140-action Diff/评论覆盖链 |
| subagent42 | `out/ely-native/subagent-42/native-run/report.json` | **passed**；48 actions |
| permission-askuser42 | `out/ely-native/permission-askuser-42/native-run/report.json` | **passed**；47 actions |
| unsupported-hooks-diagnostics42 | `out/ely-native/unsupported-hooks-diagnostics-42/native-run/report.json` | **failed**；ordinal `40` 等待项目输出 `ely-hook-supported.txt` 超时，支持 Hook 文件缺失；runner `exit_code=1`，被测进程自然退出 |
| stress-long-message-files42 | `out/ely-native/stress-long-message-files-42/native-run/report.json` | **failed**；ordinal `29` 等待“文件列表达到总量上限，展开更多目录以继续查看”超时；runner `exit_code=1`，被测进程自然退出 |
| visual-settings42 | `out/ely-native/visual-settings-42/native-run/report.json` | **passed**；24 actions，截图/UIA 取证完成，像素比较另行保持 `pending` |
| visual-workspace42 | `out/ely-native/visual-workspace-42/native-run/report.json` | **passed**；13 actions，截图/UIA 取证完成，像素比较另行保持 `pending` |
| idle-performance42 | `out/ely-native/idle-performance-42/native-run/report.json`、`out/ely-native/idle-performance-42/idle-performance-42-summary.json`、`out/ely-native/idle-performance-42/idle-identity-recheck.json` | native runner **passed**、自然退出；wrapper summary 为 `partial`，不计入 24 个正式 passed 报告，等待后续 wrapper 修复和 release43 重测 |

因此 28 个计划均已启动并留存报告，其中 24 个普通报告为 `passed`，resources、
unsupported-hooks-diagnostics 和 stress 为 `failed`，idle-performance 的 native report
虽通过但正式 wrapper 状态为 `partial`。完整 release42 产品验收仍不能写成 PASS：
runtime_native 正在修复无 Session 时的插件候选缓存，native_workbench 正在定位资源返回页，
gpui_settings 正在定位压力提示；这些修复尚未绑定新的完整验收结论。

### release42 idle 性能与身份复核

`out/ely-native/idle-performance-42/idle-performance-42-summary.json` 记录 runner
`passed`、自然退出，CPU/内存窗口为 60.303 秒、41 个指标、单核归一化 CPU `0.648392%`、
20 核归一化 `0.03242%`，user `141ms`、kernel `250ms`，working set 从 `156286976`
降至 `156168192`，private bytes 从 `201265152` 降至 `200658944`。GPU sampler 记录
55/55 有效样本、`complete=true`、`completed=0`；wrapper 因 DateTime 转换丢失小数而把
身份判定写成 `partial`。`out/ely-native/idle-performance-42/idle-identity-recheck.json`
使用 ISO/ticks 重新核对，PID、path、SHA 和 ticks 全部匹配，`validSamples=55`、
`requestedSamples=55`、runner `passed` 且自然退出；这份复核不能替代 wrapper 修复后的
正式性能报告，release43 重测前仍保持 `pending`。

### release42 视觉比较

三份全图比较均为 `acceptance=pending`、`assessment=not_identical`：

| 比较项 | 证据 | release42 结果 |
| --- | --- | --- |
| General settings | `out/ely-native/visual-settings-42/general-comparison/comparison-metrics.json` | `722742/4198400` 个像素不同，差异率 `17.2147008%`；保持 `pending` |
| Appearance | `out/ely-native/visual-settings-42/appearance-comparison/comparison-metrics.json` | `117388/4198400` 个像素不同，差异率 `2.7960175%`；保持 `pending` |
| Draft/workspace | `out/ely-native/visual-workspace-42/draft-reference8-comparison/comparison-metrics.json` | `233206/4198400` 个像素不同，差异率 `5.5546398628%`；保持 `pending` |

## release41 最终离线门禁（2026-10-06）

release41 目标 binary SHA-256 为
`360446e4b3c63b911fbac74329b78a768a8955e5e858c27289202cdefd1d3f1b`，记录见
`out/ely-release41-sha256.txt` 和 `out/ely-release-gates-41.log`。最终离线门禁的 workspace
tests 为 55 groups、`2665 passed; 0 failed; 7 ignored`。workspace/vendor fmt、fmt-check、
Clippy、release build、runner build 和 Windows production tree 均通过；release build 保留
一个 linker warning，不改变构建完成事实。source manifest 为 1550 个输入且未发现 vendor
drift。

| 范围 | 证据 | release41 判断 |
| --- | --- | --- |
| workspace tests | `out/ely-workspace-tests-41.log` | **passed**；55 groups，`2665 passed; 0 failed; 7 ignored` |
| fmt、fmt-check、Clippy | `out/ely-workspace-fmt-41.log`、`out/ely-vendor-fmt-check-41.log`、`out/ely-workspace-fmt-check-41.log`、`out/ely-workspace-clippy-41.log` | **passed** |
| release/runner build 与生产依赖树 | `out/ely-release-build-41.log`、`out/ely-runner-build-41.log`、`out/ely-windows-production-tree-41.log` | **passed**；离线构建与 Windows production tree 完成 |
| source manifest | `out/ely-source-inputs-41.json`、`out/ely-source-inputs-41.log` | **passed**；1550 inputs，无 vendor drift |

这些结果只关闭 release41 的离线 gate，不提升原生 GUI、live Provider、视觉像素或性能
验收状态；上述范围之外仍保持 `pending`。

## release41 原生 GUI 批次（pending / in progress）

`out/ely-native-batch-regression-41.log` 绑定同一 release41 binary，批次在 workbench 的
Diff 检查处停止，不能总结为 release41 完整回归通过。

| 计划 | 证据 | 当前记录 |
| --- | --- | --- |
| provider-model41 | `out/ely-native-batch-regression-41.log`、`out/ely-native/provider-model-41/native-run/report.json` | 当前批次观察到 76 actions、`status=passed`、被测进程自然退出；仅是已完成的局部计划结果 |
| workbench41 | `out/ely-native-batch-regression-41.log`、`out/ely-native/workbench-41/native-run/report.json` | 绑定 SHA `360446...d1f3b1`；140-action 计划本次执行到 65 actions，在 ordinal `64` 等待 UIA 标签“Diff 已加载”超时；进程自然退出但 runner `exit_code=1`。失败截图显示 Diff 内容和编辑 marker 已出现，但没有稳定的 UIA status 节点，因此不能把后续评论或完整 Diff 验收写成通过 |
| release41 其余 GUI 计划 | 当前没有对应报告 | 批次在 workbench 失败后停止，未运行；保持 `pending / in progress` |

当前 `tooling/native-gpui-tests/ely-native-workbench.json` 已扩展为 140 actions，新增覆盖
`Diff`、相对文件路径、`加载 Diff`、`Diff 已加载`、评论文件相对路径、行号、评论内容、
旧侧/新侧选择和 review marker 文件断言。该计划尚无独立成功报告；release41 的 65-action
中止报告不能替代它的完整运行。工作树后续已为 workbench panel status 补充稳定 ID、
`Role::Status` 与 label/value；该修订当时等待 root42 统一 gate 绑定新 binary/report，release42
当前状态以本文顶部逐项报告为准，不能据此提升为完整产品 `PASS`。

## release40 中间构建历史

release40 中间 binary SHA-256 为
`8529162169ce4f75cfc97bef1f88466df5205fcbe9b89103d744ad8362b8a112`，记录见
`out/ely-release40-sha256.txt` 和 `out/ely-release-gates-40.log`。该构建的 workspace tests
为 55 groups、`2665 passed; 0 failed; 7 ignored`；workspace/vendor fmt、fmt-check、Clippy、
release/runner build、Windows production tree 均完成，source manifest 为 1550 个输入且无
vendor drift。对应证据为 `out/ely-workspace-tests-40.log`、`out/ely-workspace-fmt-40.log`、
`out/ely-vendor-fmt-check-40.log`、`out/ely-workspace-fmt-check-40.log`、
`out/ely-workspace-clippy-40.log`、`out/ely-release-build-40.log`、
`out/ely-runner-build-40.log`、`out/ely-windows-production-tree-40.log`、
`out/ely-source-inputs-40.json` 和 `out/ely-source-inputs-40.log`。

release40 仅作为中间离线构建历史保存；它没有完整产品 GUI、live Provider、像素或性能
验收结论，不能由离线结果提升为产品 `PASS`。

## release40 sigma 视觉探针

`out/ely-native/visual-workspace-sigma-40/native-run/report.json` 绑定 release40 binary，
13 actions、进程自然退出、runner report 为 `passed`。其比较报告
`out/ely-native/visual-workspace-sigma-40/draft-reference8-comparison/comparison-metrics.json`
为 `acceptance=pending`、`assessment=not_identical`，`233206/4198400` 个像素不同，差异率
`5.5546398628%`。这是 sigma 探针，只用于观察窗口/截图链路，不属于正式全量视觉验收，也
不能提升像素或完整产品状态。

## 当前 Native typed contract 与历史边界

当前验收以 Rust Host、GPUI 投影和 Rust domain 事实源为准。Host/会话边界使用
`NativeHostApi`、`NativeUiAction`、`NativeActionReceipt` 和 `NativeEventBatch`；设置面板
使用 `apps/desktop/src/native_ui/settings/contracts.rs` 中的 `NativeSettingsError`、
`SettingsResult`、`NativeSettingsService`、`SettingsCommand`、`SettingsSnapshot` 和
`SettingsEvent`。错误只携带稳定 `code`、脱敏 `message` 与 `retryable`；面板通过 typed
`load`、`execute`、`subscribe` 取得投影，不能读取 providers.json、workflow 目录或 memory
文件制造第二份事实源。

Worktree 入口是 GPUI `NativeWorkbenchPanel` 持有的 `NativeWorktrees`，当前方法为
`list`、`create`、`remove`、`handoff`、`archive`，返回 Rust `Result` 和 typed worktree
结构；UI 中文标签不是稳定协议值。旧 renderer/Source 表单回退已经退役，仅可用于历史
diff 或迁移审查定位，不能作为当前入口、状态节点或原生验收证据。当前产品没有 Tauri、
WebView、旧 `ChannelClient`/VQL 运行时；尚未绑定新 binary 和 report 的范围必须保持
`pending`。

## release39 原生回归与视觉取证历史（2026-10-06）

release39 目标 binary SHA-256 为
`f5b9b9bbb8eeb750d650b23a9d47d52036f873e72eb029f57f2ecf897c3963d2`，记录见
`out/ely-release39-sha256.txt`。本节只记录 release39 实际运行结果；后续 release40
计划修订和重跑不改写这些失败历史。

| 计划 | 证据 | release39 结果 |
| --- | --- | --- |
| provider-model39 | `out/ely-native-batch-regression-39.log`、`out/ely-native/provider-model-39/native-run/report.json` | **failed**；61 actions，ordinal `60` 的 `providers.json` JSON Pointer `/activeModelId` 断言失败；该运行未覆盖后续补充的 `删除模型` 标签门禁，缺口保留到后续计划 |
| workbench39 | `out/ely-native-batch-regression-39b.log`、`out/ely-native/workbench-39/native-run/report.json` | **passed**；107 actions，被测进程自然退出、`exit_code=0` |
| subagent39 | 同上、`out/ely-native/subagent-39/native-run/report.json` | **passed**；48 actions，被测进程自然退出、`exit_code=0` |
| permission-askuser39 | 同上、`out/ely-native/permission-askuser-39/native-run/report.json` | **passed**；47 actions，被测进程自然退出、`exit_code=0` |
| unsupported-hooks-diagnostics39 | 同上、`out/ely-native/unsupported-hooks-diagnostics-39/native-run/report.json` | **failed**；27 actions，ordinal `26` 等待 UIA 标签“插件 ely-native-unsupported-hooks · 声明存在但当前不会执行”超时；失败报告的 UIA 树实际包含带 `@local` 的完整标签，属于计划期望值缺失后缀，不能写成产品诊断通过 |
| visual-settings39 | `out/ely-native-batch-visual-39.log`、`out/ely-native/visual-settings-39/native-run/report.json` | **passed**；24 actions，被测进程自然退出、`exit_code=0`；全图像素比较仍为 `pending` |
| visual-workspace39 | 同上、`out/ely-native/visual-workspace-39/native-run/report.json` | **passed**；13 actions，被测进程自然退出、`exit_code=0`；全图像素比较仍为 `pending` |

release39 的三份全图比较均为 `acceptance=pending`、`assessment=not_identical`，数值证据如下：

| 比较项 | 证据 | release39 结果 |
| --- | --- | --- |
| General settings comparison | `out/ely-native/visual-settings-39/general-comparison/comparison-metrics.json` | `722742/4198400` 个像素不同，差异率 `17.2147008%`；保持 `pending` |
| Appearance comparison | `out/ely-native/visual-settings-39/appearance-comparison/comparison-metrics.json` | `124830/4198400` 个像素不同，差异率 `2.9732755%`；保持 `pending` |
| Draft/workspace comparison | `out/ely-native/visual-workspace-39/draft-reference8-comparison/comparison-metrics.json` | `238040/4198400` 个像素不同，差异率 `5.6697790%`；保持 `pending` |

因此 release39 这组五项回归为 `3 passed; 2 failed`，两项视觉计划只完成截图/UIA
取证；完整产品、像素验收和性能验收继续保持 `pending`。后续 release40 的修复或重跑
必须绑定新的 binary SHA 和独立报告，不能反向提升本节 release39 结果。

## release37 离线门禁检查点（2026-10-06）

release37 目标 binary SHA-256 为
`5d3f57127a24e88456cf89c9c2ec82f1b9b4080f77b566a13b61c29a76c063bd`，证据见
`out/ely-release37-sha256.txt`。37b 的 workspace/vendor fmt、fmt check、Clippy、workspace
tests 和 doctests 均通过；workspace tests 汇总为 55 groups、`2663 passed; 0 failed; 7 ignored`。
对应日志为 `out/ely-workspace-fmt-37b.log`、`out/ely-vendor-fmt-37b.log`、
`out/ely-workspace-fmt-check-37b.log`、`out/ely-vendor-fmt-check-37b.log`、
`out/ely-workspace-clippy-37b.log`、`out/ely-workspace-tests-37b.log` 和
`out/ely-workspace-doctests-37b.log`。

`out/ely-cargo-deny-37b.log` 的 cargo-deny `0.20.2` advisories、bans、licenses、sources
均通过。release/runner build 和 Windows production tree 分别见
`out/ely-release-build-37b.log`、`out/ely-runner-build-37b.log`、
`out/ely-windows-production-tree-37b.log`；release build 有一个 `linker_messages` warning，
禁用包名匹配计数为 `0`。这些记录只覆盖离线门禁；source manifest、原生交互、视觉和性能项
仍需各自绑定对应证据。

来源 manifest 见 `out/ely-source-inputs-37.json` 和 `out/ely-source-inputs-37.log`：1550 个输入、
无 vendor drift，SOURCE 表 30/30 行哈希匹配，其中 29 个为局部文件；新增 1732、删除 300 的
统计均属于 patch 记录。

| 范围 | 当前真实证据 | release37 判断 |
| --- | --- | --- |
| 离线 workspace/依赖门禁 | 上述 37b 日志、release binary SHA 和 production tree | **passed** |
| source manifest | `out/ely-source-inputs-37.json`、`out/ely-source-inputs-37.log` | **passed**；1550 inputs，无 vendor drift，SOURCE 30/30 哈希匹配 |
| resources37 | `out/ely-native-batch-functional-37.log`、`out/ely-native/resources-37/native-run/report.json` | **passed**；272 actions，进程自然退出、`exit_code=0` |
| general-preferences37 | 同上、`out/ely-native/general-preferences-37/native-run/report.json` | **passed**；171 actions，进程自然退出、`exit_code=0` |
| provider-settings37 | 同上、`out/ely-native/provider-settings-37/native-run/report.json` | **passed**；119 actions，进程自然退出、`exit_code=0` |
| keybindings37 | 同上、`out/ely-native/keybindings-37/native-run/report.json` | **passed**；61 actions，进程自然退出、`exit_code=0` |
| keybindings-cold-resume37 | 同上、`out/ely-native/keybindings-cold-resume-37/native-run/report.json` | **passed**；35 actions，进程自然退出、`exit_code=0` |
| goal-workflow37 | 同上、`out/ely-native/goal-workflow-37/native-run/report.json` | **passed**；68 actions，进程自然退出、`exit_code=0` |
| goal-lifecycle37 | 同上、`out/ely-native/goal-lifecycle-37/native-run/report.json` | **passed**；108 actions，进程自然退出、`exit_code=0` |
| automation37 | 同上、`out/ely-native/automation-37/native-run/report.json` | **passed**；75 actions，进程自然退出、`exit_code=0` |
| subagent37 | 同上、`out/ely-native/subagent-37/native-run/report.json` | **failed**；29 actions，ordinal `28` 等待 `transcript_segment_committed` 超时；进程自然退出，runner `exit_code=1`、`processExit=0` |
| full37 | `out/ely-native-batch-remaining-37.log`、`out/ely-native/full-37/native-run/report.json` | **passed**；63 actions，进程自然退出、`exit_code=0` |
| cold-resume37 | 同上、`out/ely-native/cold-resume-37/native-run/report.json` | **passed**；35 actions，进程自然退出、`exit_code=0` |
| permission-askuser37 | 同上、`out/ely-native/permission-askuser-37/native-run/report.json` | **failed**；29 actions，ordinal `28` 等待 `turn_completed` 超时；`termination=killed-after-timeout`，runner `exit_code=1`、`processExit=1` |
| stop-reopen37 | `out/ely-native-batch-rest-37.log`、`out/ely-native/stop-reopen-37/native-run/report.json` | **passed**；53 actions，进程自然退出、`exit_code=0` |
| workbench37 | 同上、`out/ely-native/workbench-37/native-run/report.json` | **failed**；49 actions，ordinal `48` 保存 UTF-8 文件时 UIA 子树不可用；`processExit=-1073740791`，报告 `exit_code=1` |
| code-settings37 | `out/ely-native-batch-regressions-37.log`、`out/ely-native/code-settings-37/native-run/report.json` | **passed**；71 actions，进程自然退出、`exit_code=0` |
| code-settings-cold-resume37 | 同上、`out/ely-native/code-settings-cold-resume-37/native-run/report.json` | **passed**；26 actions，进程自然退出、`exit_code=0` |
| navigation37 | 同上、`out/ely-native/navigation-37/native-run/report.json` | **passed**；108 actions，进程自然退出、`exit_code=0` |
| hooks37 | 同上、`out/ely-native/hooks-37/native-run/report.json` | **passed**；123 actions，进程自然退出、`exit_code=0` |
| hooks-cold-resume37 | 同上、`out/ely-native/hooks-cold-resume-37/native-run/report.json` | **passed**；37 actions，进程自然退出、`exit_code=0` |
| provider-model37 | 同上、`out/ely-native/provider-model-37/native-run/report.json`、`network-trace.jsonl` | **failed**；17 actions，ordinal `16` UIA 等待“模型真实生成测试通过”超时 `176369ms`。该设置页模型测试独立于会话 Journal，`turn_started=0` 和终态事件 `0` 属于预期；网络请求为 success/HTTP `200`，但 UIA 未观察到成功 notice |
| settings37（深色全设置） | `out/ely-native-batch-settings-rest-37.log`、`out/ely-native/settings-37/native-run/report.json` | **passed**；102 actions，进程自然退出、`exit_code=0` |
| settings-light37（浅色全设置） | 同上、`out/ely-native/settings-light-37/native-run/report.json` | **passed**；102 actions，进程自然退出、`exit_code=0` |
| font-settings37 | 同上、`out/ely-native/font-settings-37/native-run/report.json` | **passed**；72 actions，进程自然退出、`exit_code=0` |
| unsupported-hooks-diagnostics37 | 同上、`out/ely-native/unsupported-hooks-diagnostics-37/native-run/report.json` | **failed**；19 actions，ordinal `18` 等待 UIA 标签“当前 Native 执行入口未实现”超时；进程自然退出、`processExit=0`，报告 `exit_code=1` |
| visual-settings37 | `out/ely-native-batch-visual-37.log`、`out/ely-native/visual-settings-37/native-run/report.json` | **passed**；24 actions，进程自然退出、`exit_code=0`；像素比较仍为 `pending` |
| visual-workspace37 | 同上、`out/ely-native/visual-workspace-37/native-run/report.json` | **passed**；13 actions，进程自然退出、`exit_code=0`；像素比较仍为 `pending` |
| release37 原生批次汇总 | 上述六组批次日志及 26 份 `report.json` | `21 passed; 5 failed`；失败项保留各自报告状态 |
| 完整产品 | release37 原生批次 | `pending`；provider-model 的 HTTP `200` 只证明独立模型测试请求有效，不能替代完整会话 UI 回合证据 |
| 性能 | release37 性能证据尚未绑定 | `pending` |

## release38 离线门禁与视觉检查点（截至 2026-10-06）

release38/38b 的编译失败已修复。release38 binary SHA-256 为
`1f60c607818e4b970a51b90fe2bc11109bf4110c1b29a69455537eff065ae602`，记录见
`out/ely-release38-sha256.txt`。38g 完整离线门禁已完成：workspace tests 为 55 groups、
`2665 passed; 0 failed; 7 ignored`；vendor editor 测试为 `20 passed; 0 failed`；workspace/vendor
fmt、fmt check、Clippy、doctests 和 cargo-deny advisories/bans/licenses/sources 均通过。对应证据为
`out/ely-workspace-tests-38g.log`、`out/ely-vendor-editor-tests-38g.log`、
`out/ely-workspace-fmt-38g.log`、`out/ely-vendor-fmt-38g.log`、
`out/ely-workspace-fmt-check-38g.log`、`out/ely-vendor-fmt-check-38g.log`、
`out/ely-workspace-clippy-38g.log`、`out/ely-workspace-doctests-38g.log` 和
`out/ely-cargo-deny-38g.log`。release build、runner build 和 Windows production tree 分别见
`out/ely-release-build-38g.log`、`out/ely-runner-build-38g.log` 和
`out/ely-windows-production-tree-38g.log`；release build 保留一个 linker warning，不改变构建完成事实。

release38 source manifest 见 `out/ely-source-inputs-38.json`、`out/ely-source-inputs-38.log`：1550
个输入、无 vendor drift。`out/ely-vendor-source-audit-38f.json` 记录 SOURCE 30/30 行哈希匹配，
其中 29 个为 patch，`added=1883`、`deleted=275`。

38c workspace tests 曾挂起，root 核实 PID `4720` 后终止进程，记录见
`out/ely-workspace-test-stall-38c.json`；这不改写 38g 最终门禁结果。Provider offline test 已
seed catalog，但没有新的产品门禁结论；当前观察也不能把 runtime stack 根因宣称为已 100% 证实。
editor38d 编译失败；editor38e 实际记录为 `19 passed; 1 failed`，失败为
`replacing_a_multiline_selection_invalidates_the_previous_rows`，证据为
`out/ely-vendor-editor-tests-38e.log`。editor38f 的 vendor editor 测试为 `20 passed; 0 failed`，
使用真实 GPUI 首帧和输入模拟验证两项新回归，证据为 `out/ely-vendor-editor-tests-38f.log`；这些
editor 结果只覆盖 vendor editor。

| 范围 | 当前真实证据 | release38 判断 |
| --- | --- | --- |
| visual-settings38 | `out/ely-native-batch-visual-38.log`、`out/ely-native/visual-settings-38/native-run/report.json` | **passed**；24 actions，进程自然退出、`exit_code=0` |
| visual-workspace38 | 同上、`out/ely-native/visual-workspace-38/native-run/report.json` | **passed**；13 actions，进程自然退出、`exit_code=0` |
| General settings comparison | `out/ely-native/visual-settings-38/general-comparison/comparison-metrics.json` | `acceptance=pending`、`assessment=not_identical`；753210/4198400 个像素不同，差异率 `17.9404058689%` |
| Appearance comparison | `out/ely-native/visual-settings-38/appearance-comparison/comparison-metrics.json` | `acceptance=pending`、`assessment=not_identical`；155268/4198400 个像素不同，差异率 `3.6982660061%` |
| Workspace comparison | `out/ely-native/visual-workspace-38/draft-reference8-comparison/comparison-metrics.json` | `acceptance=pending`、`assessment=not_identical`；239063/4198400 个像素不同，差异率 `5.6941453887%` |
| 其他 release38 功能计划 | 当前没有对应原生报告 | 未运行，不记录为 PASS；完整产品和未覆盖项保持 `pending` |

当前正在进行 release39 toolbar 参数调整，因此 release38 binary 和上述证据不代表调整后的源码；
release37 的失败历史保持原样。

## release36 离线门禁与原生批次检查点（2026-10-06）

release36 目标 binary SHA-256 为
`14894efe6301874e91e6a2b34e9af3dfc5847fc71e62e8ea02dd7c54fcd121eb`。36b 的 fmt、fmt
check、Clippy、workspace tests 和 doctests 均有完成记录：`out/ely-workspace-tests-36b.log`
汇总为 55 groups、`2660 passed; 0 failed; 7 ignored`，对应门禁日志为
`out/ely-workspace-fmt-check-36b.log`、`out/ely-workspace-clippy-36b.log` 和
`out/ely-workspace-doctests-36b.log`。36c 使用 `out/tools/cargo-deny/bin` 的 cargo-deny
`0.20.2`，`out/ely-cargo-deny-36c.log` 的 advisories、bans、licenses、sources 均通过；
release/runner build 和 Windows production tree 分别见
`out/ely-release-build-36c.log`、`out/ely-runner-build-36c.log`、
`out/ely-windows-production-tree-36c.log`，禁用包名为 `0`。来源 manifest 见
`out/ely-source-inputs-36.json` 和 `out/ely-source-inputs-36.log`：1544 个输入、无 vendor
drift，SOURCE 表 30 行、29 行 patch。

| 范围 | 当前真实证据 | release36 判断 |
| --- | --- | --- |
| Navigation36 | `out/ely-native-batch-functional-A-36.log`、`out/ely-native/navigation-36/native-run/report.json` | **failed**；ordinal `46` 在前进 click 后没有出现 UIA 标签“任务通知”，进程自然退出、`exit_code=0` |
| Hooks36 | `out/ely-native-batch-functional-B-36.log`、`out/ely-native/hooks-36/native-run/report.json` | **passed**；123 actions，进程自然退出、`exit_code=0` |
| Hooks cold-resume36 | `out/ely-native-batch-functional-B-36.log`、`out/ely-native/hooks-cold-resume-36/native-run/report.json` | **failed**；34 actions，ordinal `33` 删除 Hook 后 trust 仍非空 |
| functional-C36 / provider-model | `out/ely-native-batch-functional-C-36.log`、`out/ely-native/provider-model-36/native-run/report.json`、`network-trace.jsonl` | **failed**；ordinal `16` 等待“模型真实生成测试通过”超时；真实网络已收到 HTTP `200` 和 `MessageEnd`，但 complete 可能仍等待 EOF，不能判定通过 |
| functional-D36 / provider-settings | `out/ely-native-batch-functional-D-36.log`、`out/ely-native/provider-settings-36/native-run/report.json` | **passed**；119 actions，进程自然退出、`exit_code=0` |
| functional-D36 / code-settings | `out/ely-native-batch-functional-D-36.log`、`out/ely-native/code-settings-36/native-run/report.json` | **failed**；44 actions，ordinal `43` 的 `Space` 持久化为 `true` 断言失败，尚未走到 `Enter` |
| functional-E36 / keybindings | `out/ely-native-batch-functional-E-36.log`、`out/ely-native/keybindings-36/native-run/report.json` | **passed**；61 actions，进程自然退出、`exit_code=0` |
| functional-E36 / keybindings cold-resume | `out/ely-native-batch-functional-E-36.log`、`out/ely-native/keybindings-cold-resume-36/native-run/report.json` | **passed**；35 actions，进程自然退出、`exit_code=0` |
| 视觉取证 | `out/ely-native-batch-visual-36.log`、visual-settings/workspace36 报告 | 24/13 actions 均通过、自然退出 `0`；三份像素比较全部 `pending` |
| 完整产品 | release36 门禁和当前原生批次 | `pending`；Navigation、Hooks cold-resume、provider-model 和 code-settings 已失败 |
| 像素与性能 | release36 比较 metrics、性能证据 | `pending` |

后续 release37 代码修复不属于 release36 证据，不能提前写成 PASS；release35 及更早检查点
继续保留原有历史判断。

## release35 离线门禁与原生批次验收（2026-10-06）

release35 目标 binary 的 SHA-256 为
`e66a3872a2219821722dbb0e6b6facc4b65c05f2e883c17f10aa43949efb506d`，证据见
`out/ely-release35-sha256.txt`。`out/ely-workspace-tests-35b.log` 含 55 个
`test result:`，合计 `2659 passed; 0 failed; 7 ignored`。`out/ely-release-gates35b.log`
只记录末段 vendor/native 测试的 `28 passed; 0 failed`，不能替代完整 workspace 汇总，
也不应与完整汇总相加；release 构建仍有一个 `linker_messages` warning。
`out/ely-windows-production-tree-35b.log`
记录 `keencode-desktop v0.0.1` 生产依赖树，未命中 Tauri、Wry、WebView2、Electron、
Node.js 或 JavaScript engine 关键字。上述内容只属于 release35 的离线构建与依赖审计，
不代表功能批次通过。

| 范围 | 当前真实证据 | release35 判断 |
| --- | --- | --- |
| Navigation | `out/ely-native/navigation-35/native-run/report.json`；binary SHA 与 release35 一致 | **failed**；ordinal `46` 等待 UIA 标签“任务通知”超时，错误为 `等待 UIA 标签超时：任务通知`；报告为 failed，但被测进程自然退出、`exit_code=0`，诊断截图和 UIA JSON 已保留 |
| functional-B35 / hooks | `out/ely-native-batch-functional-B-35.log`、`out/ely-native/hooks-35/native-run/report.json` | **failed**；ordinal `70` 等待 `hooks-user-marker.txt` 超时，进程自然退出、`exit_code=0` |
| functional-C35 / provider-model | `out/ely-native-batch-functional-C-35.log`、`out/ely-native/provider-model-35/native-run/report.json` | **failed**；ordinal `13` 缺少 JSON Pointer `/providers/0/disabledModels` |
| functional-C35b / provider-model 重跑 | `out/ely-native-batch-functional-C-35b.log`、`out/ely-native/provider-model-35b/native-run/report.json` | **failed**；ordinal `13` 的 `/providers/0/models` 不匹配。隔离源 `providers.json` 实际有 4 个模型，而计划按 1 个模型断言；这是计划/fixture 假设不匹配，不能据此判定业务功能错误 |
| functional-D35 / provider-settings | `out/ely-native-batch-functional-D-35.log`、`out/ely-native/provider-settings-35/native-run/report.json` | **failed**；ordinal `45` 等待 UIA 标签“当前模型”超时，进程自然退出、`exit_code=0` |
| functional-E35 | `out/ely-native-batch-functional-E-35.log` 及 resources、general-preferences、code-settings、cold-resume、keybindings 报告 | **failed**；resources、general-preferences、code-settings、cold-resume 分别通过 `272/171/63/26` actions；keybindings ordinal `32` 断言目标文件未包含要求文本 `native-keybindings.json`，目标文件实际存在，属于内容断言不匹配 |
| 视觉取证 | `out/ely-native-batch-visual-35.log`、`out/ely-native-batch-visual-workspace-35b.log` | visual-settings 通过，visual-workspace 首次失败、重跑通过；仅为取证，像素比较仍 `pending` |
| 完整产品 | release35 离线门禁、原生批次和 navigation 诊断 | `pending`；上述证据不能覆盖失败或尚未完成的完整产品范围 |
| 像素 | 当前 release35 目标截图与同条件比较报告 | `pending`；不能由构建、依赖树或导航自然退出提升 |

产品裁剪边界保持不变：界面语言切换仍是 native 验收的未完成项，`N-17` 保持
`pending`；Office 模式和主动任务推荐已裁剪，兼容记录固定为 `interfaceMode=coding`，
不把缺失控件计为原生功能失败或像素要求。release33 及更早报告不覆盖 release35 或
当前工作树；C35b 需先修订模型数量断言，再以自身报告和同一 binary 复测。

## release33 原生验收（2026-10-06）

本批报告均绑定 release33 binary SHA-256
`b3841d8a26e286110396bb6bae43fdb5985f5b36fde44b03391de7fd0d592215`。构建证据为
`out/ely-release-build-33d.log`、`out/ely-runner-build-33d.log` 和
`out/ely-release33-sha256.txt`。以下结论只属于该 binary，不代表已有后续变化的
release34 源码树；release34 已包含 Hooks 补齐与旧 benchmark 清理。

| 范围 | 当前真实证据 | 判断 |
| --- | --- | --- |
| Provider 设置与 CRUD | `out/ely-native/provider-settings-33/native-run/report.json`，119 actions，进程自然退出、exit code `0` | PASS，仅覆盖该计划 |
| Visual 设置取证 | `out/ely-native/visual-settings-33/native-run/report.json`，24 actions，4 份截图、3 份 UIA、4 份 metrics，GPU 为 `pending` | PASS（取证）；像素仍 pending |
| Code 设置 | `out/ely-native/code-settings-33/native-run/report.json`，63 actions，进程自然退出、exit code `0` | PASS，仅覆盖该计划 |
| Code 设置冷恢复 | `out/ely-native/code-settings-cold-resume-33/native-run/report.json`，26 actions，进程自然退出、exit code `0` | PASS，仅覆盖该计划 |
| Goal / Workflow 真实 actor | `out/ely-native/goal-workflow-33/native-run/report.json`，68 actions，Journal 新增 `turn_started=1`、终态事件 `1` | PASS，仅覆盖该计划 |
| Goal 生命周期 | `out/ely-native/goal-lifecycle-33/native-run/report.json`，108 actions，Journal 新增 `turn_started=2`、终态事件 `2` | PASS，仅覆盖该计划 |
| Automation 生命周期 | `out/ely-native/automation-33/native-run/report.json`，75 actions，Journal 新增 `turn_started=2`、终态事件 `2` | PASS，仅覆盖该计划 |
| 资源 CRUD | `out/ely-native/resources-33/native-run/report.json`，257 actions 均 passed，但本轮无新增 Turn，报告整体 failed | failed |
| General 偏好 | `out/ely-native/general-preferences-33/native-run/report.json`，51 actions；1 个 UIA bounds 为 `(1981,-181)-(2169,-135)`，与 `2560x1640` client 无交集 | failed |
| Keybindings | `out/ely-native/keybindings-33/native-run/report.json`，56 actions；要求新增 `turn_started=1`，观察到 `2`，最终 `killed-after-timeout` | failed |
| Subagent（附加运行） | `out/ely-native/subagent-33/native-run/report.json`，20 actions；等待新增 `turn_started` 超时 | failed |

release33 构建门禁：`out/ely-workspace-tests-33d.log` 记录 2636 passed、0 failed、
7 ignored、55 groups；`out/ely-workspace-clippy-33d.log` 完成 Clippy/编译检查；
`out/ely-vendor-choices-tests-33e.log` 记录 17 passed。门禁结果不能沿用到当前
release34 源码树。`out/ely-native-batch-functional-33.log` 只作批次摘要，具体状态以
各原生 `report.json` 为准。

## release32 原生验收（2026-10-06）

本批原生报告均绑定 release32 binary SHA-256
`95f3778547682d46412345a21d8261c0920512d934f036ae6d6e2d8db26ccee7`，构建证据为
`out/ely-release-build-32.log` 和 `out/ely-release32-sha256.txt`。

| 范围 | 当前真实证据 | 判断 |
| --- | --- | --- |
| Goal / Workflow 真实 actor | `out/ely-native/goal-workflow-32/native-run/report.json`，68 actions，自然退出 0，Journal `turn_started=1`、终态事件 `1` | PASS，仅覆盖该计划 |
| Goal 生命周期 | `out/ely-native/goal-lifecycle-32/native-run/report.json`，108 actions，自然退出 0，Journal `turn_started=2`、终态事件 `2` | PASS，仅覆盖该计划 |
| Automation 生命周期 | `out/ely-native/automation-32/native-run/report.json`，75 个计划动作均 passed；报告整体因收尾 Ctrl+Q 的 `SetForegroundWindow` Win32 error 1400 为 failed | failed |
| Provider CRUD | `out/ely-native/provider-settings-32/native-run/report.json`；等待 UIA 标签 `Messages` 超时 | failed，等待 release33 复测 |
| 资源 CRUD | `out/ely-native/resources-32/native-run/report.json`；等待 UIA 标签 `ely-resource-mcp` 超时 | failed，等待 release33 复测 |
| General 偏好 | `out/ely-native/general-preferences-32/native-run/report.json`；目标 UIA bounds 与 client 无交集 | failed，等待 release33 复测 |
| Keybindings | `out/ely-native/keybindings-32/native-run/report.json`；等待本轮新增 `turn_started` 超时 | failed，等待 release33 复测 |
| 视觉取证、像素与性能 | `out/ely-native/visual-settings-32/native-run/report.json`，24 actions 的截图/UIA 取证；GPU 未采样 | pending，所有视觉验收保持 pending |

`out/ely-workspace-tests-32d.log` 的 2634 passed、`out/ely-workspace-clippy-32f.log`
的通过记录、`out/ely-runner-tests-32.log` 的 39 passed 以及 vendor 离线测试，只是
release32 构建门禁。release32 之后已有源码变更，不能把 2634/clippy 通过沿用为当前
工作树新版本的验收结果；必须针对新源码重新执行。所有视觉验收仍为 `pending`。

## 最新检查点：release31（2026-10-06）

分支 `feat/ely-native`；binary SHA-256
`1e59a14e260a5997c0150a3332f56275e12945ae9b9671c4b68027f53d7061c0`。
`out/ely-workspace-clippy-31b.log`、格式检查和 release/runner 构建通过；
`out/ely-workspace-tests-31.log` 记录 2628 passed、0 failed、7 ignored、55 groups。
Windows 生产依赖树未匹配 Tauri/Wry/WebView2/JS 引擎。

| 范围 | 当前真实证据 | 判断 |
| --- | --- | --- |
| 文件编辑、PTY、Git/worktree、搜索 | `out/ely-native/workbench-31/native-run/report.json`，107 actions，自然退出 0 | PASS，仅覆盖该计划 |
| Goal / Workflow 真实 actor | `out/ely-native/goal-workflow-31/native-run/report.json`，68 actions，自然退出 0 | PASS，仅覆盖该计划 |
| Provider CRUD | `out/ely-native/provider-settings-31/native-run/report.json`；创建已落盘，编辑 UIA 名称歧义失败 | failed，修复待新 binary |
| 资源 CRUD | `out/ely-native/resources-31/native-run/report.json`；MCP 已落盘，标题 UIA 等待失败 | failed，修复待新 binary |
| Goal 生命周期 | `out/ely-native/goal-lifecycle-31/native-run/report.json`；blocked reason JSON Pointer 断言失败 | failed，待复测 |
| General / Appearance 截图 | `out/ely-native/visual-settings-31/native-run/report.json`，24 actions，自然退出 0 | 取证 PASS；像素 pending |
| 最终产品与性能 | schema v2 代码设置、完整编辑器换行、General 偏好和控件校正尚未统一构建验收 | pending |

以上通过项只绑定 release31，不能代替后续源码验证。历史失败报告保留原状。

## 历史检查点：release30（2026-10-06）

当前分支为 `feat/ely-native`，release30 binary SHA-256 为 `4e83c5fa2e117b63d11e2312be386c57095b603d9a7f9fd0e22549d5cf95b784`。
格式、Clippy 和 workspace tests 均通过；测试日志 `out/ely-workspace-tests-30c.log`
记录 2608 passed、0 failed、7 ignored。此后 Appearance、Terminal、Goal 生命周期与
工作台修复仍在开发，不能将 release30 的通过项归到尚未构建的新版本。

| 范围 | 当前真实证据 | 判断 |
| --- | --- | --- |
| Goal 保存与真实 Workflow actor | `out/ely-native/goal-workflow-30b/native-run/report.json`，68 actions；包含真实权限批准、artifact-committed 和 run-settled(succeeded)；脱敏网络 trace 2928 bytes | PASS，仅覆盖该计划 |
| 文件编辑、PTY、Git/worktree | `out/ely-native/workbench-30/native-run/report.json`；前置 PTY、保存、暂存、提交通过，worktree 数量断言失败 | failed，Windows Git 扩展路径参数修复待新 binary 复测 |
| Provider 创建 | `out/ely-native/provider-settings-30b/native-run/report.json`；诊断截图 `out/ely-native/provider-create-diagnostic-30/native-run/screenshots/0046-provider-create-result-top.bmp` 显示新模型缺视觉能力配置 | failed，typed handler 的默认 false 修复待新 binary 复测 |
| 普通 parent/child Agent | `out/ely-native/subagent-30c/native-run/report.json`，48 actions；单层 child Read、spawn/wait 和父子终态强断言通过，turn_started=2、终态=2，脱敏 trace 16838 bytes | PASS，仅覆盖 release30 的该计划；30/30b 失败报告保留 |
| 像素与资源占用 | 最新目标全图比较仍为 release28；新 Appearance 与资源导航尚未留存目标截图，最终 PID 性能未采样 | pending |

固定 Ely 源码已更新至 `94f34c9f8e98b5f4b3078776a4c197b9021f4cdf`；上游工作树干净。
完整产品验收仍为 pending；下方历史记录不覆盖本检查点的未验证范围。

## 历史诊断与执行记录

## 状态规则

本矩阵只接受真实 Windows GPUI/Ely 窗口、真实键鼠输入、真实 Rust Host 状态和可复核
证据。`session-calibration-12` 已执行真实窗口校准并观察到一次 `session_created`，
但被测进程随后因 `gpui::app::entity_map::double_lease_panic` 崩溃，runner 报告
`GetWindowRect 失败`；这不是产品功能验收，因此所有产品运行项仍为 `pending`。已有
浏览器、离线单测、domain 测试或旧版原生报告不能替代本矩阵的证据。

`PASS` 必须同时有动作结果、截图或 UIA 证据、相关 Journal/网络证据以及被测 binary
身份；失败、超时、缺少真实 Provider 或 trace 缺失均不得改写为通过。

此前 `smoke-15` 的 `report.json.status=passed` 只证明真实 Provider 请求与 Journal
回合链完成：本轮有 `turn_started` 1 个、终态事件 1 个、network trace `5466` bytes，
进程自然退出。截图 `0014-completed.bmp` 已显示真实回复正文，但 Composer 输入未清空并
出现 `hostdraft error`，因此这不能提升完整产品验收；N-03、N-04 及依赖完整回合显示的
产品项仍保持 `pending`。其中 Provider 配置加载或授权本身不等于已发出模型请求。

随后 `smoke-20` 的 `report.json.status=failed`：输入和键盘动作有记录，但 Enter 后
没有新增 `turn_started`，最终因 Journal 等待超时失败；配套 `0009-typed.bmp` 显示
输入前缀丢字。该失败证据不能提升任何产品项，产品、像素和性能验收仍保持 `pending`。

此前 `smoke-21` 的 `report.json.status=passed`，Journal 记录了完整 prompt、assistant
`native smoke ok`、`turn_started=1` 和 `turn_completed=1`；隔离磁盘 draft 的 `text` 已为
空字符串，network trace 为 `5466` bytes，进程自然退出且 `exit_code=0`。这只证明真实
Provider、Enter 提交和清稿链通过。`0017-completed.bmp` 的正文区域仍为空，后续 chat flex
高度修复正在进入 build22 验证，因此完整聊天 UI、产品功能、像素和性能验收继续保持
`pending`。

随后 `smoke-22` 的 `report.json.status=failed`：虽然报告中的 click/type/key action 均为
`passed`，但没有新增 `turn_started`，最终因等待 Journal 超时；network trace 与
`model-request-records.jsonl` 均为 `0` bytes，进程自然退出且 `exit_code=0`。只有
`screenshots/0011-typed.bmp` 和诊断图 `input-crop.png`，结论是输入未命中失败，不能把动作
记录视为真实输入提交成功，产品、像素和性能验收仍保持 `pending`。

`smoke-22b` 使用同一 release22 binary（SHA-256
`17668ba0f3fc2eb25f02aae5b791f4d5e83b41fb79822d105f26cafcb3b8a19a`）并通过真实 Provider
回合链：`turn_started=1`、`terminal_event=1`（终态 Journal 事件为 `turn_completed`），network trace 为
`5466` bytes，model request records 为 `3145` bytes；Journal 含真实用户 prompt 和
assistant `native smoke ok`。`0011-typed.bmp`、`0014-started.bmp`、`0017-completed.bmp`
以及 `metrics/0018-completed.json` 均生成，进程自然退出且 `exit_code=0`；隔离 draft
`native-drafts/e4d422832dda039d3663db30fdad84ccb7de1bb1a22a29c4476731c76dbe6156.json` 的
`text` 为空，`0017-completed.bmp` 的正文可见。这只证明真实模型请求、Enter 提交、Journal
回合和清稿链通过；完整产品功能、像素和性能验收仍为 `pending`，metrics 的 GPU 状态也仍为
`pending`。

release24 binary SHA-256 为
`56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f`。`smoke-24` 的
`report.json.status=passed`，计划为 `out/ely-release24-live-ui-plan.json`；真实 Shift+Enter
预填、Enter 提交和空白 hitarea 均通过，Journal 有 `turn_started=1`、`terminal_event=1`，
network trace 为 `5466` bytes，model request records 为 `3145` bytes，截图
`0015-typed.bmp`、`0018-started.bmp`、`0021-completed.bmp` 和 metrics
`0022-completed.json` 均生成。完成截图正文可见且 Composer 清空；报告列出本轮 draft 文件，
但运行未保留 isolation，因此不把 draft 的精确持久化正文作为独立证据。该运行只证明最小
真实请求/提交/Journal/清稿链，完整产品功能、像素和性能仍为 `pending`，GPU 仍为 `pending`。

`full-24` 绑定同一 release24 binary，真实 Read/Write 的 `tool_requested`、
`tool_execution_started`、`tool_completed` 事件和文件 marker
`KEENCODE_NATIVE_ACCEPTANCE_MARKER_20261005` 均已出现，但本轮没有 `turn_completed`；报告
因等待终态超时为 `failed`，`turn_started=1`、`terminal_event=0`，进程
`exit_code=1` 且 `termination=killed-after-timeout`。运行时只读诊断表明
`collect_model_stream` 已返回，且 `execute_tools` 的审批路径先于对应的 `tool_requested`；
Journal 只记录了已落盘的工具事件，没有记录具体第三个 tool，因此无法确定该 tool 的实际
审批结果。失败阶段仅有 `accessibility/0005-startup.json` 和
`accessibility/0012-session-created.json` 两棵 UIA 树，helper、provider 和 subtree 均可用；
`0015-permission-options.bmp`、`0018-automatic-edit-selected.bmp` 和
`0025-turn-started.bmp` 只有截图，没有 post-failure UIA 树或 `turn-completed` 取证。这只
记录工具链诊断证据，完整产品项仍为 `pending`。

`stop-reopen-24c` 是当前最新的停止/重开原生证据：报告 `status=passed`，计划为
`tooling/native-gpui-tests/ely-native-stop-reopen.json`，绑定 release24 binary SHA-256
`56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f`。Journal 有
`turn_started=2`、`terminal_event=2`，其中实际 `turn_stopped=1` 且 payload 的 `reason=cancelled`，
第二轮有 `turn_completed=1`；`events.jsonl` 为 18354 bytes，脱敏 network trace 为 13850 bytes，
进程自然退出且 `exit_code=0`。`0025-before-stop.bmp`、`0030-after-stop.bmp`、
`0036-session-reopened.bmp` 和 `0050-second-round-completed.bmp`，以及对应的 UIA/metrics
证据均已生成。这只证明停止、会话重开和第二轮回合链通过，不提升未覆盖的产品、像素或性能项。

`settings-dark-24` 初次运行因真实验收缺少非空 Session Journal 而失败，Journal 和 network
trace 均为 `0` bytes；修复后的 `settings-dark-24b` 报告为 `passed`，八个设置页
（General、Providers、Resources、Automations、Workflows、Agents、Usage、Diagnostics）均有
截图/UIA 证据，并有 `turn_started=1`、`terminal_event=1`、`turn_completed=1` 和
`5466` bytes network trace。这只证明设置页和最小真实回合链，完整产品、像素和性能仍为
`pending`。

release25 binary SHA-256 为
`2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452`。`full-25` 报告为
`status=passed`，计划为 `tooling/native-gpui-tests/ely-native-full.json`；Journal 记录
`turn_started=2`、`terminal_event=2`、`turn_completed=2`，并记录 3 次
`tool_requested`、`tool_execution_started`、`tool_completed`，包含 Read 和两次 Write，
两次写入 marker 分别为 `KEENCODE_NATIVE_ACCEPTANCE_MARKER_20261005` 和
`KEENCODE_NATIVE_ACCEPTANCE_SECOND_MARKER_20261005`。本轮 Journal 事件文件合计 `25847`
bytes，脱敏 network trace 为 `19638` bytes，model request records 为 `11180` bytes；9 份
截图、6 份 UIA 和 6 份 metrics 均生成，进程自然退出且 `exit_code=0`。这只证明 release25
工具、会话重开和第二轮回合链，完整产品、像素和性能仍为 `pending`。

`settings-dark-25` 与 `settings-light-25` 均绑定上述 release25 binary，报告均为
`status=passed`，计划分别为 `tooling/native-gpui-tests/ely-native-settings.json` 和
`tooling/native-gpui-tests/ely-native-settings-light.json`。深色和浅色两轮均覆盖 General、
Providers、Resources、Automations、Workflows、Agents、Usage、Diagnostics 八个设置页，
并有 `turn_started=1`、`terminal_event=1`、`turn_completed=1`、`5466` bytes network trace、
`3145` bytes model request records，进程自然退出且 `exit_code=0`。这只证明 release25 的
深浅设置页和最小真实回合链，不能提升全部产品、像素或性能项。

`release25-calibration` 报告为 `status=passed`，计划为 `out/ely-release25-calibration.json`，
绑定同一 release25 binary；窗口和截图/metrics 链通过，比较指标记录与固定 reference6 的
`2560x1640` client 有 `314065` 个不同像素，差异比例为 `7.4805878%`，且
`acceptance=pending`、`assessment=not_identical`，因此不能视为像素通过。

`permission-askuser-25` 绑定同一 release25 binary，报告为 `failed`：UIA helper 使用默认
DPI 导致 `click_accessibility` 坐标偏移，后续等待“自动编辑”标签超时；该失败属于 runner
取证/坐标问题，Journal 没有新增 Turn，不能解释为产品 Permission 功能失败或通过。

上述新增结果均属于 release25 SHA-256
`2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452`，不代表 release26，也不代表像素验收通过。

release26 当前被测 binary SHA-256 为
`2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94`。
`font-settings-26` 是首次 runner 失败：普通坐标 click 后状态未稳定，最后 `assert_file`
未找到 `native-general-settings.json`；它不能作为字体功能结论。修正后的
`font-settings-26b` 报告为 `status=passed`，计划为
`tooling/native-gpui-tests/ely-native-font-settings.json`，通过 UIA 定位并验证输入
`Consolas`、应用、离开后重开设置页、清空输入并应用系统默认回退；最终隔离文件的
`fontFamily` 为 `系统默认`，7 份截图、4 份 UIA、进程自然退出且 `exit_code=0`。这只证明
字体设置持久化/回退链，不提升像素或完整产品验收。

`permission-askuser-26c` 绑定同一 release26 binary，报告为 `failed`。它已启动真实 Turn，
但没有终态事件，脱敏 network trace 为 `0` bytes；`0021-permission-pending.bmp` 与对应
UIA 树显示长 JSON 工具消息把聊天内容撑宽，`允许`/`拒绝` UIA bounds 落在窗口 client
之外，后续 `click_accessibility` 报告 bounds 与 client 无交集并超时。该运行暴露的是
`gpui_chat` 的长 JSON 布局/权限按钮 offscreen 产品问题，审批结果不能从本轮报告推出。

`workbench-26` 绑定同一 release26 binary，报告为 `failed`：进入工作台并切换终端后，
等待“新建终端”的 UIA 标签超时（exact=0），未形成 Journal 或网络证据；进程虽自然退出、
`exit_code=0`，不构成工作台通过。fixture Git 同时向父仓库解析出过大 diff，
`native_workbench` 仍在排查，不能把本轮写成产品或 Git 功能通过。

`release26-calibration` 也绑定上述 release26 binary，报告为 `status=passed`，计划仍为
`out/ely-release25-calibration.json`；窗口和截图/metrics 链通过，与固定 reference6 的
`2560x1640` client 有 `314065` 个不同像素，差异比例为 `7.4805878%`，比较文件仍记录
`acceptance=pending`、`assessment=not_identical`。它只构成 release26 的视觉校准证据，
不提升像素、产品或性能验收状态。

上述 release26 功能记录只描述各自的设置、权限和工作台范围；`release26-calibration` 仅为
视觉校准，比较仍为 `pending`，没有像素或完整产品 `PASS`。

release27 当前被测 binary SHA-256 为
`3d4412d7937f80b23ef36699594483c073b9c0cef250cc7da24c34eee3f1649c`。
`permission-askuser-27` 报告为 `status=passed`，计划为
`tooling/native-gpui-tests/ely-native-permission-askuser.json`；两轮 Turn 均完成，记录
`turn_started=2`、`tool_requested=2`、`turn_completed=2`，network trace `14154` bytes，
Journal 事件 `19364` bytes，model request records `8103` bytes。第一轮精确消息为“请调用 Bash
工具执行 printf 'KEENCODE_PERMISSION_MARKER_20261005' > permission-marker.txt，然后简短回答。”，
点击 `允许` 后 marker 文件为 35 bytes、内容精确且无换行；第二轮 AskUser 精确问题为“请选择
原生验收选项（可多选）。”，仅有 A/B 两个选项，`multiSelect=true`、`allowCustom=false`，
选择 A 并提交后完成。运行生成 5 份截图、5 份 UIA 和 1 份 metrics，进程自然退出、
`exit_code=0`。这只证明 Permission/AskUser 范围，不能提升像素或完整产品验收。

`workbench-27` 与 `workbench-27b` 均绑定上述 release27 binary 并报告为 `failed`。27 已完成
工作台、终端、“新建终端”定位和终端创建截图，随后等待项目输出 `terminal-marker.txt` 超时；
27b 已进一步生成终端创建和命令输入截图/UIA，仍在等待同一 marker 时超时。两轮 Journal 和
network trace 均为 `0` bytes，进程均自然退出、`exit_code=0`；终端高度相关根因待后续复测，
不能把这两轮写成工作台或 Git 功能通过。

release27 的记录只描述 Permission/AskUser 和工作台范围；没有像素或完整产品 `PASS`。

release28 当前被测 binary SHA-256 为
`a902bc39845758ecab43fc2d6a415877de277b97226edd7be19979649cb09e1b`。
`workbench-28` 的终端和 UIA 输入链已走到等待 marker，但 `terminal-marker.txt` 超时；根因记录
为扩展 `\\?\` 盘符 cwd 未被 CMD 接受并回退到 `C:\Windows`，预期项目目录没有 marker 或 `.git`
写入。`goal-workflow-28` 已落盘 Goal JSON，但点击“执行”时 UIA 找不到唯一节点
（`exact=0 invalidBounds=0 nodesTruncated=false`）；根因记录为 scope Combobox 缺少
`.selected`，默认 `global` 被清空。两份报告均保持 `failed`，不能提升 Workbench/Git 或
Goal/Workflow 产品项。

`release28-calibration` 的窗口、截图和 metrics 链为 `PASS`，但与固定 reference6 的视觉比较
仍记录 `acceptance=pending`、`assessment=not_identical`；它只构成 release28 的视觉校准证据。

最新 Ely GPUI Components 来源已拉取，当前依赖固定在
`718617939e1c4f938f21751f1fdbfe2ec230188f`（许可证和来源见
`THIRD_PARTY_NOTICES.md`）。最新 UI 代码尚未完成编译或原生验收；上述 release24 报告不能
被解释为最新 UI 已通过。

## 视觉基线边界

`out/native-live/zcode-source-baseline-29628c9-dpr2/` 中的 dpr2 PNG 是固定 ZCode 3.14.3
来源提交的浏览器内容参考：报告记录为 Playwright Chromium context 加 Vite web shell，
`source-appearance-dpr1.json` 使用 HeadlessChrome UA。它没有 HWND、原生 window/client
矩形或 Windows 最大化证据；`workspace-dpr2.json` 的 `window-maximized` 只是 DOM class。

该浏览器路径的壳层几何是 `radius=12px`、`inset=0px`。Windows 原生目标应按
`workspaceShellWindowChrome.ts` 的 Windows 分支和 `WorkspaceShellLayout.tsx` 的桌面 inset
使用 `radius=5px`、`inset=4px`，对应原生 `apps/desktop/src/native_ui/style.rs::DESKTOP_INSET`。
源图的侧栏、正文列和 Composer 数值仍可用于内容边界校准，但不能用来宣称 Windows 原生
外壳或像素 `PASS`。

当前安装的 `D:\dev\ZCode\ZCode.exe` FileVersion 为 `3.14.4.7912`，不是 fixed 3.14.3，
不能将其截图作为同源原生基线。固定来源的真实 Windows reference-5 已记录在
`out/ely-native/zcode-windows-reference-5/native-run/report.json`；它只作为同源窗口和
页面状态参照，不等于目标功能验收。原生目标还必须遵守 `surface` Light
`#0d0d0d @ 3%`、Dark `#ffffff @ 5%`，`sunken`/`overlay`/输入为 `#ffffff` /
`#2b2b2b`，以及 Windows `CONVERSATION_SCROLL_GUTTER=15px` 逻辑像素。缺少同主题、同
窗口、同 DPI、同字体、同页面状态的目标 diff 前，视觉项保持 `pending`。

## 工具自身检查

| 检查 | 命令 | 状态 | 证据边界 |
| --- | --- | --- | --- |
| Rust 格式 | `cargo fmt --manifest-path tooling/native-gpui-tests/Cargo.toml -- --check` | PASS | 验收器源码格式检查通过 |
| 宿主 target 单包检查 | `cargo check -p keencode-native-gpui-tests` | PASS | 只证明当前 Windows 主机 target 的依赖和编译路径 |
| Windows target 单包检查 | `cargo check -p keencode-native-gpui-tests --target x86_64-pc-windows-msvc` | PASS | 只证明 Windows API 类型和链接声明可编译 |
| 生产 Host 构建（release15） | `cargo build -p keencode-desktop --release` | PASS | `out/ely-release-build-15.log`；release build 在 1m04s 完成；calibration16 report 记录了实际 binary SHA |
| Workspace tests（batch 12） | 测试记录 | PASS | `out/ely-workspace-tests-12.log`；记录的测试组均为 `test result: ok` |
| Workspace Clippy（18） | Clippy 记录 | PASS | `out/ely-workspace-clippy-18.log`；检查完成 |
| Workspace Clippy（19） | Clippy 记录 | PASS | `out/ely-workspace-clippy-19.log`；检查完成 |
| Workspace tests（batch 23） | 测试记录 | PASS | `out/ely-workspace-tests-23.log`；记录的测试组均为 `test result: ok`，包括 `599 passed; 0 failed; 4 ignored`、`214 passed; 0 failed; 1 ignored` 和 `224 passed; 0 failed`；仅为离线检查 |
| Workspace Clippy（23b） | Clippy 记录 | PASS | `out/ely-workspace-clippy-23b.log`；完成 `keencode-desktop` 编译，`Finished dev profile [unoptimized + debuginfo] target(s) in 6.00s`；仅为离线检查 |
| 生产 Host 构建（release24） | `cargo build -p keencode-desktop --release` | PASS | `out/ely-release-build-24.log`；release binary 由原生 report 绑定 SHA-256 `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f` |
| Workspace tests（batch 24） | 测试记录 | failed | `out/ely-workspace-tests-24.log`；`598 passed; 1 failed; 4 ignored`，失败为 `app_exit::tests::idle_host_starts_cleanup_without_confirmation` 的固定 20ms 调度假设；根因已修复，待重测 |
| Workspace Clippy（24 初次） | Clippy 记录 | failed | `out/ely-workspace-clippy-24.log`；初次检查命中 `manual_strip` 与 `type_complexity`，不能作为通过 |
| Workspace Clippy（24b） | Clippy 记录 | PASS | `out/ely-workspace-clippy-24b.log`；`keencode-desktop` 编译完成，`Finished dev profile [unoptimized + debuginfo] target(s) in 5.60s` |
| Native runner tests（25/25b） | 测试记录 | PASS（25b） | `out/ely-runner-tests-25.log` 初次为 `23 passed; 1 failed`，原因是 JSONL 测试夹具缺少换行；修复后 `out/ely-runner-tests-25b.log` 为 `24 passed; 0 failed` |
| 生产 Host 原生运行 | `cargo run -p keencode-native-gpui-tests -- ...` | pending | 必须绑定真实 binary、隔离 Provider 和输出报告 |

## 功能矩阵

| ID | 验收范围 | 最小真实动作 | 必须保存的证据 | 状态 |
| --- | --- | --- | --- | --- |
| N-01 | 原生窗口创建 | 启动 binary，等待进程 PID 的可见顶层 HWND | `report.json.process`、startup BMP、窗口标题/类名 | pending |
| N-02 | AccessKit/UIA surface | 读取根节点与子窗口边界 | `accessibility/startup.json`；provider 状态和根属性 | pending |
| N-03 | 真实 prompt | Tab/点击输入区，输入短 prompt，Enter 提交 | 输入动作结果、提交后截图、Journal 事件 | pending |
| N-04 | 流式响应 | 使用隔离真实 Provider，观察 streaming 到终态 | streaming/终态截图、消息事件和 seq | pending |
| N-05 | 停止 | 流式期间 Escape/停止按钮，等待活动回合结束 | stop 前后截图、终态事件、无活跃回合断言 | pending |
| N-06 | 历史 | 完成一轮后重新打开会话并读取历史 | 历史截图、Journal 恢复事件、消息数量 | pending |
| N-07 | 冷恢复 | 关闭 Host，再从同一隔离 data/project 启动 | 两次 binary 身份、冷启动截图、无重复消息证据 | pending |
| N-08 | Read | 在隔离项目读取 `src/facts.txt` | 工具调用/结果事件、文件内容 marker、截图 | pending |
| N-09 | Write | 写入隔离项目中的新文件并重新读取 | 文件字节/路径、工具事件、截图 | pending |
| N-10 | Bash | 在隔离项目执行无副作用命令 | 命令、退出码、stdout 摘要和 Journal 事件 | pending |
| N-11 | Permission | 触发需要授权的工具动作，选择允许/拒绝 | permission surface UIA、选择前后截图、终态事件 | pending |
| N-12 | Question/elicitation | 触发问题面板并提交回答 | 问题 surface UIA、回答事件、stream 终态 | pending |
| N-13 | Goal | 创建、运行、取消或完成一个隔离 goal | goal 状态截图、Journal 状态迁移和终态 | pending |
| N-14 | Subagents | 通过 UI 触发一个受限 child agent 并等待父子终态 | 父子 turn/actor 事件、资源边界和截图 | pending |
| N-15 | Workflows | 创建或执行一个最小 workflow，并验证取消/终态 | workflow JSON 版本、节点事件、active barrier 证据 | pending |
| N-16 | Resources | 打开资源面板，创建、读取、更新、删除隔离资源 | UIA/截图、资源 Journal、冷恢复结果 | pending |
| N-17 | Config/settings | 打开设置，修改并重新读取模型/权限/locale 等配置 | 设置截图、配置摘要、重启回读证据 | pending |
| N-18 | Worktrees | 在隔离仓库执行创建、切换、交接或归档 | Git 路径和状态摘要、界面截图、Journal | pending |
| N-19 | Native editor | 打开编辑器并修改隔离文本 | 编辑器 UIA、文件字节、保存后截图 | pending |
| N-20 | PTY/terminal | 打开终端并运行隔离项目命令 | PTY surface 截图、退出码、终端/工具事件 | pending |
| N-21 | Long message | 输入边界内长 prompt 与长回复，观察布局和滚动 | 多阶段截图、消息字节/长度摘要、终态事件 | pending |
| N-22 | Large directory | 在隔离 project 生成大目录并打开浏览/搜索入口 | 文件数量/索引摘要、窗口截图、CPU/private bytes | pending |
| N-23 | Repeated open-close | 重复打开、聚焦、关闭原生窗口并重新启动 | 每轮 HWND/exit code、截图、无残留进程 | pending |
| N-24 | Idle | 完成回合后等待固定窗口并采样 | 多次 CPU/private memory、无活跃任务和日志错误 | pending |

## 证据矩阵

| ID | 证据 | 判定条件 | 状态 |
| --- | --- | --- | --- |
| E-01 | Journal | Rust Journal 事件、seq/resync、session/message/tool/workflow 终态可回放；验收器只列隔离文件清单 | pending |
| E-02 | Network trace | `KEENCODE_NATIVE_WIRE_TRACE` 有限 JSONL 存在，认证 Header、key、token 和凭据 URL 已脱敏 | pending |
| E-03 | Screenshots | startup、stream、stop、permission/question、settings、tool 结果和 close 阶段均有 BMP | pending |
| E-04 | Accessibility | 每个关键 surface 有 UIA root/property/child evidence，且明确 provider 是否存在 | pending |
| E-05 | CPU/private memory | metrics JSON 至少包含 startup、stream、stop、idle；报告采样时刻和进程 PID | pending |
| E-06 | GPU | 有 Windows GPU Engine/ETW 或等价受控采样，能绑定 PID 和采样窗口 | pending |
| E-07 | Isolation | Provider 配置、data、project、输出路径均在本次隔离根；报告不含凭据值 | pending |
| E-08 | Binary provenance | 报告记录 binary 绝对路径和 SHA-256；构建 feature、时间与外部构建日志绑定 | pending |

## 执行记录

每次运行均需补充报告路径、binary SHA、执行时间和实际状态。校准失败或缺少真实
Provider 时，产品项仍保留 `pending`，不以计划文件被成功解析或窗口短暂出现作为通过。

| 执行批次 | binary / SHA | Provider 配置路径 | 报告 | 结果 |
| --- | --- | --- | --- | --- |
| Ely native smoke | 待执行 | `out/native-live/native-provider-6f2.json`（仅路径） | 待生成 | pending |
| `zcode-capture-1` 视觉比较（2026-10-05 20:02:22–20:02:30 +08:00） | `target/release/keencode-desktop.exe` / `96707a2509bdf2f6ae9362c48f5583aa88b1afaff8c95d61b0693f9615088b84` | 无（capture-only） | `out/ely-native/zcode-capture-1/visual-comparison/comparison-metrics.json` | pending；client 区裁切为 `2560x1640`，不同像素 `980308/4198400`（`23.3496%`）；源图是 browser `radius=12px/inset=0px`，原生目标为 `radius=5px/inset=4px`，且页面状态不同，不能作为像素通过 |
| `session-calibration-12`（2026-10-05 17:51:59 +08:00） | `target/release/keencode-desktop.exe` / `e4bacee3d1bef1a7eb02a248c7b13b928150c606931b006cc61ca00991112768` | 无（仅窗口/Journal 校准） | `out/ely-native/session-calibration-12/native-run/report.json` | failed；真实窗口和 `session_created` 成功，随后 `double_lease_panic` 崩溃 |
| `session-calibration-13`（2026-10-05 18:19:36 +08:00） | 报告未生成，binary 身份未留存 | 无（仅窗口/Journal 校准） | `out/ely-native/session-calibration-13/native-run/` | failed；`double_lease` 未再出现，窗口和 `session_created` 成功；UIA 取证卡死，runner 未生成 `report.json`，仅保留 startup log、截图和 metrics |
| `session-calibration-14`（2026-10-05 18:22:04 +08:00） | `target/release/keencode-desktop.exe` / `42f206a4ed89fa8e93bf3aef07f1caa0ad23130cc9c2f62ac854587929ce04c3` | 无（仅窗口/Journal 校准） | `out/ely-native/session-calibration-14/native-run/report.json` | failed；窗口、`session_created`、截图和 metrics 成功；UIA/关闭阶段 5 秒超时后由 runner 终止，Journal 无 Turn，network trace 为 0 bytes |
| `session-calibration-15`（2026-10-05 18:32:22 +08:00） | `target/release/keencode-desktop.exe` / `42f206a4ed89fa8e93bf3aef07f1caa0ad23130cc9c2f62ac854587929ce04c3` | 无（仅窗口/Journal 校准） | `out/ely-native/session-calibration-15/native-run/report.json` | failed；窗口、`session_created`、截图和 metrics 成功；UIA/关闭阶段 5 秒超时后由 runner 终止，Journal 无 Turn，network trace 为 0 bytes |
| `session-calibration-16`（2026-10-05 18:35:36 +08:00） | `target/release/keencode-desktop.exe` / `f90374f38069df3cf598ccd05387dc321a5ac516502feef64a93913cad8f4641` | 无（仅窗口/Journal 校准） | `out/ely-native/session-calibration-16/native-run/report.json` | failed；窗口、`session_created`、截图和 metrics 成功；UIA/关闭阶段 5 秒超时后由 runner 终止，Journal 无 Turn，network trace 为 0 bytes |
| `session-calibration-17`（2026-10-05 18:48:47 +08:00） | `target/release/keencode-desktop.exe` / `f90374f38069df3cf598ccd05387dc321a5ac516502feef64a93913cad8f4641` | 无（仅窗口/Journal 校准） | `out/ely-native/session-calibration-17/native-run/report.json` | failed；窗口、`session_created`、截图和 metrics 成功；关闭等待超时，report/process-output 均记录 `termination=killed-after-timeout`，Journal 无 Turn，network trace 为 0 bytes |
| `session-calibration-18`（2026-10-05 18:49:31 +08:00） | `target/release/keencode-desktop.exe` / `f90374f38069df3cf598ccd05387dc321a5ac516502feef64a93913cad8f4641` | 无（仅窗口/UIA 校准） | `out/ely-native/session-calibration-18/native-run/report.json` | failed；UIA provider 可见但 helper 在 `uia-find:start` 处 5 秒超时，subtree 为 unavailable；随后关闭等待超时并记录 `termination=killed-after-timeout`，Journal 无 Turn，network trace 为 0 bytes |
| `smoke-1`（2026-10-05 18:24:53 +08:00） | `target/release/keencode-desktop.exe` / `42f206a4ed89fa8e93bf3aef07f1caa0ad23130cc9c2f62ac854587929ce04c3` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/smoke-1/native-run/report.json` | failed；真实输入和 Enter 提交动作完成，但 root prep 在 Provider 请求前因 Turn ID `native-ui:send:18db9b2daf428c38-2` 含 `:` 被资源层拒绝；Journal 记录失败，network trace 与 model request records 均为 0 bytes |
| `release15`（2026-10-05 18:33:10–18:34:15 +08:00） | `target/release/keencode-desktop.exe` / `f90374f38069df3cf598ccd05387dc321a5ac516502feef64a93913cad8f4641`（由 calibration16 report 绑定） | 无 | `out/ely-release-build-15.log` | PASS（release build，耗时 1m04s）；修复后的 `native_root_turn_id` 尚未用新 binary 做原生 smoke 复验 |
| `session-calibration-19`（2026-10-05 18:56:35–18:56:39 +08:00） | `target/release/keencode-desktop.exe` / `5e3d737c6a17882e965d7d4f4939aa4927db0884fb33ad69849f6dcd86decc67` | 无（仅窗口/Journal 校准） | `out/ely-native/session-calibration-19/native-run/report.json` | PASS（真实新 Session、`session_created` 和截图/metrics 成功；Ctrl+Q 后进程自然退出）；这是校准证据，不提升产品功能项状态 |
| `smoke-2`（2026-10-05 18:56:52–18:57:17 +08:00） | `target/release/keencode-desktop.exe` / `5e3d737c6a17882e965d7d4f4939aa4927db0884fb33ad69849f6dcd86decc67` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/smoke-2/native-run/report.json` | failed；真实 Provider 已授权，输入和 Enter、`turn_started` 均成功，但 process output 报告 `系统找不到指定的文件。 (os error 2)`，runner 超时终止；未形成完成回合，network trace 为 0 bytes |
| `smoke-3`（2026-10-05 19:05:37–19:08:42 +08:00） | `target/release/keencode-desktop.exe` / `5e3d737c6a17882e965d7d4f4939aa4927db0884fb33ad69849f6dcd86decc67` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/smoke-3/native-run/report.json` | failed；真实 Provider analytics 成功并完成输入/`turn_started`，但没有助手消息和 `turn_completed`，runner 等待该事件超时后终止；不能作为完整流式回合通过 |
| `idle-baseline-1`（2026-10-05 19:03:23–19:03:41 +08:00） | `target/release/keencode-desktop.exe` / `5e3d737c6a17882e965d7d4f4939aa4927db0884fb33ad69849f6dcd86decc67` | 无（空闲基线） | `out/ely-native/idle-baseline-1/native-run/report.json` | PASS（15 秒空闲采样；单核 CPU 约 `0.205%`，private bytes `171.92–174.98 MiB`）；仅为空闲基线，完整性能验收仍为 `pending` |
| `zcode-calibration-5`（2026-10-05 20:26:59–20:27:02 +08:00，root runner 12 相关） | `target/release/keencode-desktop.exe` / `96707a2509bdf2f6ae9362c48f5583aa88b1afaff8c95d61b0693f9615088b84` | 无（ZCode 几何校准） | `out/ely-native/zcode-calibration-5/native-run/report.json` | PASS（`focus`、窗口尺寸断言、截图和 metrics 均通过；window `2586x1653`、client `2560x1640`、DPI `192`；进程自然退出、exit code `0`）；仅证明窗口/截图/metrics 校准，产品项仍为 `pending` |
| `session-calibration-21`（2026-10-05 20:29:20–20:29:22 +08:00） | `target/release/keencode-desktop.exe` / `96707a2509bdf2f6ae9362c48f5583aa88b1afaff8c95d61b0693f9615088b84` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/session-calibration-21/native-run/report.json` | failed；仅 `click` 和 `session_created` 通过，real-provider 计划末尾要求真实 Turn，但本轮无 `TurnStarted`/终态事件且 network trace、model request records 均为 `0`；进程自然退出、exit code `0`。配置加载/授权不计为模型请求 |
| `zcode-calibration-6`（2026-10-05 20:30:15–20:30:17 +08:00，root runner 12 相关） | `target/release/keencode-desktop.exe` / `96707a2509bdf2f6ae9362c48f5583aa88b1afaff8c95d61b0693f9615088b84` | 无（ZCode 几何校准） | `out/ely-native/zcode-calibration-6/native-run/report.json` | PASS（`focus`、窗口尺寸断言、截图和 metrics 均通过；window `2586x1653`、client `2560x1640`、DPI `192`；进程自然退出、exit code `0`）；仅证明窗口/截图/metrics 校准，产品项仍为 `pending` |
| `smoke-14`（2026-10-05 20:30:25–20:30:39 +08:00） | `target/release/keencode-desktop.exe` / `96707a2509bdf2f6ae9362c48f5583aa88b1afaff8c95d61b0693f9615088b84` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/smoke-14/native-run/report.json` | PASS（仅证明真实 Provider + Journal 回合链：`turn_started=1`、终态事件 `1`、network trace `5476` bytes；截图 `0013-completed.bmp` 和 metrics `0014-completed.json` 均生成；进程自然退出、exit code `0`）；该截图仍正文为空且 Composer 未清空，N-03、N-04 等完整产品项继续 `pending` |
| `smoke-15`（2026-10-05 20:53:48–20:53:55 +08:00） | `target/release/keencode-desktop.exe` / `6649d8d173f56213e591853719829fc9a3541a3d8452f796da31124a00e1cade` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/smoke-15/native-run/report.json` | PASS（仅证明真实 Provider + Journal 回合链：`turn_started=1`、终态事件 `1`、network trace `5466` bytes；截图 `0014-completed.bmp` 与 metrics `0015-completed.json` 均生成，真实回复正文可见；Composer 输入未清空并出现 `hostdraft error`，因此完整产品项仍为 `pending`；进程自然退出、exit code `0`） |
| `smoke-20` | `target/release/keencode-desktop.exe` / `868fc686fcd213ece93811a8787a6af0e40513f6e01c9fa3c298d2badee17967` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/smoke-20/native-run/report.json` | failed；输入截图 `0009-typed.bmp` 显示前缀丢字；Enter 后未产生新增 `turn_started`，最终因 Journal 等待超时失败，network trace 与 model request records 均为 `0` |
| `smoke-21` | `target/release/keencode-desktop.exe` / `38f60fe3b03f7c551ac721fdd864106407507aa56680e18baaac636ae742a41b` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/smoke-21/native-run/report.json` | PASS（仅证明模型/Enter/清稿链：Journal 有完整 prompt 和 assistant `native smoke ok`，`turn_started=1`、`turn_completed=1`，draft `native-drafts/642f3309ef9cc9f6e2ca460f356e2ad3d349e153cfae36396b07ba97b047f0ec.json` 的 `text` 为空，network trace `5466` bytes；截图 `0017-completed.bmp` 正文仍空白，完整聊天 UI、产品、像素和性能继续 `pending`；metrics `0018-completed.json`，进程自然退出、exit code `0`） |
| `release21-calibration` | `target/release/keencode-desktop.exe` / `38f60fe3b03f7c551ac721fdd864106407507aa56680e18baaac636ae742a41b` | 无（视觉校准） | `out/ely-native/release21-calibration/native-run/report.json`；`visual-comparison/comparison-metrics.json` | PASS（窗口、截图和 metrics 生成）；同平台首屏与固定 reference-5 比较差异 `8.4274%`，metrics `acceptance=pending`、`assessment=not_identical`；不同 greeting 时间和内容的历史 `18.6916%` 也不能宣称像素通过，视觉和性能仍为 `pending` |
| `smoke-22` | `target/release/keencode-desktop.exe` / `17668ba0f3fc2eb25f02aae5b791f4d5e83b41fb79822d105f26cafcb3b8a19a` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/smoke-22/native-run/report.json`；`process-output.json` | failed；click/type/key action 虽为 `passed`，但输入未命中，未产生新增 `turn_started`，等待 Journal 超时；`0011-typed.bmp` 和 `input-crop.png` 已保存，network trace 与 model request records 为 `0` bytes，进程自然退出、exit code `0` |
| `smoke-22b` | `target/release/keencode-desktop.exe` / `17668ba0f3fc2eb25f02aae5b791f4d5e83b41fb79822d105f26cafcb3b8a19a` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/smoke-22b/native-run/report.json` | PASS（使用 `out/ely-release22-live-ui-plan.json`；仅证明真实模型请求/Enter/Journal 回合/清稿链：`turn_started=1`、`terminal_event=1`、`turn_completed=1`，network trace `5466` bytes，model request records `3145` bytes；`0011-typed.bmp`、`0014-started.bmp`、`0017-completed.bmp`、`metrics/0018-completed.json` 生成，draft `native-drafts/e4d422832dda039d3663db30fdad84ccb7de1bb1a22a29c4476731c76dbe6156.json` 的 `text` 为空且完成正文可见；进程自然退出、exit code `0`；完整产品、像素和性能仍 `pending`，GPU 仍 `pending`） |
| `release24-calibration` | `target/release/keencode-desktop.exe` / `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f` | 无（视觉校准） | `out/ely-native/release24-calibration/native-run/report.json`；`visual-comparison/comparison-metrics.json` | PASS（窗口、截图和 metrics 生成；与 reference6 的全图比较差异 `7.5209%`，metrics `acceptance=pending`、`assessment=not_identical`；仅为视觉证据，不能宣称像素通过，产品和性能仍 `pending`） |
| `smoke-24` | `target/release/keencode-desktop.exe` / `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/smoke-24/native-run/report.json` | PASS（计划 `out/ely-release24-live-ui-plan.json`；Shift+Enter 预填、Enter 提交和空白 hitarea 通过，`turn_started=1`、`terminal_event=1`、`turn_completed=1`，network trace `5466` bytes，model request records `3145` bytes；截图 `0015-typed.bmp`、`0018-started.bmp`、`0021-completed.bmp`、metrics `0022-completed.json` 生成，完成正文可见且 Composer 清空；未保留 isolation，不宣称 draft 精确持久化正文；完整产品、像素和性能仍 `pending`，GPU 仍 `pending`） |
| `full-24` | `target/release/keencode-desktop.exe` / `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/full-24/native-run/report.json` | failed；Read/Write 真实工具事件和 marker `KEENCODE_NATIVE_ACCEPTANCE_MARKER_20261005` 已出现；`collect_model_stream` 已返回，运行时只读诊断显示 `execute_tools` 审批路径先于对应 `tool_requested`，但 Journal 未记录具体第三 tool，无法确定实际审批结果；`turn_started=1`、`terminal_event=0`，等待 `turn_completed` 超时，进程 `exit_code=1`、`termination=killed-after-timeout`；仅有 startup/session-created UIA，失败阶段截图没有 post-failure UIA 树，完整产品项仍 `pending` |
| `stop-reopen-24c` | `target/release/keencode-desktop.exe` / `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/stop-reopen-24c/native-run/report.json` | PASS（计划 `tooling/native-gpui-tests/ely-native-stop-reopen.json`；`turn_started=2`、真实 `turn_stopped=1` 且 `reason=cancelled`、`turn_completed=1`，`terminal_event=2`，network trace `13850` bytes，进程自然退出、exit code `0`；`0025-before-stop.bmp`、`0030-after-stop.bmp`、`0036-session-reopened.bmp`、`0050-second-round-completed.bmp` 及对应 UIA/metrics 已生成；只证明停止/重开链，未提升全部产品项） |
| `settings-dark-24` | `target/release/keencode-desktop.exe` / `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/settings-dark-24/native-run/report.json` | failed；八页截图/UIA 动作已生成，但真实验收缺少非空 Session Journal，Journal 与 network trace 均为 `0` bytes，不能视为设置验收通过 |
| `settings-dark-24b` | `target/release/keencode-desktop.exe` / `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/settings-dark-24b/native-run/report.json` | PASS（八个设置页 General/Providers/Resources/Automations/Workflows/Agents/Usage/Diagnostics 均有截图与 UIA，且最小真实回合 `turn_started=1`、`terminal_event=1`、`turn_completed=1`、network trace `5466` bytes；只证明设置页和最小真实回合链，完整产品、像素和性能仍 `pending`，GPU 仍 `pending`） |
| `full-25` | `target/release/keencode-desktop.exe` / `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/full-25/native-run/report.json` | PASS（计划 `tooling/native-gpui-tests/ely-native-full.json`；`turn_started=2`、`terminal_event=2`、`turn_completed=2`，3 次工具调用，Journal 事件 `25847` bytes，network trace `19638` bytes，进程自然退出、exit code `0`；只证明 release25 工具/会话重开链，完整产品、像素和性能仍 `pending`） |
| `settings-dark-25` | `target/release/keencode-desktop.exe` / `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/settings-dark-25/native-run/report.json` | PASS（计划 `tooling/native-gpui-tests/ely-native-settings.json`；深色八页和最小真实回合，`turn_started=1`、`terminal_event=1`、`turn_completed=1`，network trace `5466` bytes，进程自然退出；只证明设置范围，完整产品、像素和性能仍 `pending`） |
| `settings-light-25` | `target/release/keencode-desktop.exe` / `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/settings-light-25/native-run/report.json` | PASS（计划 `tooling/native-gpui-tests/ely-native-settings-light.json`；浅色八页和最小真实回合，`turn_started=1`、`terminal_event=1`、`turn_completed=1`，network trace `5466` bytes，进程自然退出；只证明设置范围，完整产品、像素和性能仍 `pending`） |
| `release25-calibration` | `target/release/keencode-desktop.exe` / `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452` | 无（视觉校准） | `out/ely-native/release25-calibration/native-run/report.json`；`visual-comparison/comparison-metrics.json` | PASS（窗口、截图和 metrics 生成；与 reference6 比较差异 `7.4805878%`，metrics `acceptance=pending`、`assessment=not_identical`；仅为视觉证据，不是像素验收通过） |
| `permission-askuser-25` | `target/release/keencode-desktop.exe` / `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/permission-askuser-25/native-run/report.json` | failed；UIA helper 默认 DPI 导致点击坐标偏移，等待“自动编辑”标签超时；属于 runner 取证失败，Journal 无新增 Turn，不能作为产品 Permission 结论 |
| `font-settings-26` | `target/release/keencode-desktop.exe` / `2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94` | 无（设置校准） | `out/ely-native/font-settings-26/native-run/report.json` | failed；首次 runner 普通坐标 click 后状态未稳定，`assert_file` 未找到 `native-general-settings.json`；不能作为字体功能结论 |
| `font-settings-26b` | `target/release/keencode-desktop.exe` / `2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94` | 无（设置校准） | `out/ely-native/font-settings-26b/native-run/report.json` | PASS（通过 UIA 验证 Consolas 输入应用、离开重开和清空回退；最终 `fontFamily=系统默认`，进程自然退出、exit code `0`；仅为设置范围证据） |
| `permission-askuser-26c` | `target/release/keencode-desktop.exe` / `2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/permission-askuser-26c/native-run/report.json` | failed；真实 `turn_started=1` 但无终态、network trace `0` bytes；长 JSON 撑宽聊天布局，允许/拒绝 bounds 落到窗口外，点击失败；属于当前产品布局问题，不能推出审批结果 |
| `workbench-26` | `target/release/keencode-desktop.exe` / `2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94` | 无（工作台校准） | `out/ely-native/workbench-26/native-run/report.json` | failed；终端切换后等待“新建终端” UIA 标签超时，Journal 与 network trace 均为 `0` bytes；fixture Git 向父仓库发现过大 diff，native_workbench 继续排查 |
| `release26-calibration` | `target/release/keencode-desktop.exe` / `2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94` | 无（视觉校准） | `out/ely-native/release26-calibration/native-run/report.json`；`visual-comparison/comparison-metrics.json` | PASS（窗口、截图和 metrics 生成；与 reference6 比较差异 `7.4805878%`，metrics `acceptance=pending`、`assessment=not_identical`；仅为视觉证据，不是像素验收通过，产品和性能仍 `pending`） |
| `permission-askuser-27` | `target/release/keencode-desktop.exe` / `3d4412d7937f80b23ef36699594483c073b9c0cef250cc7da24c34eee3f1649c` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/permission-askuser-27/native-run/report.json` | PASS（两轮 Turn 完成，`turn_started=2`、`tool_requested=2`、`turn_completed=2`，network trace `14154` bytes，Journal `19364` bytes，精确 Permission 消息触发 marker 文件；AskUser 严格 A/B 多选并提交 A；仅为范围证据，完整产品和像素仍 `pending`） |
| `workbench-27` | `target/release/keencode-desktop.exe` / `3d4412d7937f80b23ef36699594483c073b9c0cef250cc7da24c34eee3f1649c` | 无（工作台校准） | `out/ely-native/workbench-27/native-run/report.json`；`process-output.json` | failed；终端创建和输入动作完成，但等待 `terminal-marker.txt` 超时；Journal 与 network trace 均为 `0` bytes，进程自然退出、exit code `0`；终端高度根因待复测 |
| `workbench-27b` | `target/release/keencode-desktop.exe` / `3d4412d7937f80b23ef36699594483c073b9c0cef250cc7da24c34eee3f1649c` | 无（工作台校准） | `out/ely-native/workbench-27b/native-run/report.json`；`process-output.json` | failed；复测已生成终端创建和命令输入截图/UIA，但等待 `terminal-marker.txt` 仍超时；Journal 与 network trace 均为 `0` bytes，进程自然退出、exit code `0`；终端高度根因待复测 |
| `workbench-28` | `target/release/keencode-desktop.exe` / `a902bc39845758ecab43fc2d6a415877de277b97226edd7be19979649cb09e1b` | 无（工作台校准） | `out/ely-native/workbench-28/native-run/report.json`；`process-output.json` | failed；等待 `terminal-marker.txt` 超时；CMD 未接受扩展 `\\?\` 盘符 cwd 并回退到 `C:\Windows`，隔离项目没有 marker 或 `.git` 写入；Journal 与 network trace 均为 `0` bytes，进程自然退出、exit code `0` |
| `goal-workflow-28` | `target/release/keencode-desktop.exe` / `a902bc39845758ecab43fc2d6a415877de277b97226edd7be19979649cb09e1b` | `out/native-live/native-provider-6f2.json`（仅路径） | `out/ely-native/goal-workflow-28/native-run/report.json` | failed；点击 UIA 标签“执行”超时，`exact=0 invalidBounds=0 nodesTruncated=false`；Journal `373` bytes、network trace `0` bytes，进程自然退出、exit code `0`；Goal 已落盘但无新的 Workflow 文件，scope Combobox `.selected`/默认 `global` 根因仍需绑定修复后复测 |
| `release28-calibration` | `target/release/keencode-desktop.exe` / `a902bc39845758ecab43fc2d6a415877de277b97226edd7be19979649cb09e1b` | 无（视觉校准） | `out/ely-native/release28-calibration/native-run/report.json`；`visual-comparison/comparison-metrics.json` | PASS（窗口、截图和 metrics 生成，进程自然退出、exit code `0`）；与 reference6 的全图比较差异 `7.4781107%`，`313961/4198400` 个像素不同，metrics 为 `acceptance=pending`、`assessment=not_identical`；仅为视觉校准证据，产品和像素仍 `pending`） |

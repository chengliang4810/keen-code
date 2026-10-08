# Ely/GPUI 原生 Windows 验收说明

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

release39 目标 binary 的 SHA-256 为
`f5b9b9bbb8eeb750d650b23a9d47d52036f873e72eb029f57f2ecf897c3963d2`，记录见
`out/ely-release39-sha256.txt`。本节只保存 release39 实际运行结果；后续 release40
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

release37 目标 binary 的 SHA-256 为
`5d3f57127a24e88456cf89c9c2ec82f1b9b4080f77b566a13b61c29a76c063bd`，记录见
`out/ely-release37-sha256.txt`。37b 的 workspace/vendor fmt、fmt check、Clippy、workspace
tests 和 doctests 均完成；证据见 `out/ely-workspace-fmt-37b.log`、
`out/ely-vendor-fmt-37b.log`、`out/ely-workspace-fmt-check-37b.log`、
`out/ely-vendor-fmt-check-37b.log`、`out/ely-workspace-clippy-37b.log`、
`out/ely-workspace-tests-37b.log` 和 `out/ely-workspace-doctests-37b.log`。workspace tests
共有 55 groups，合计 `2663 passed; 0 failed; 7 ignored`。

37b 使用 cargo-deny `0.20.2`；`out/ely-cargo-deny-37b.log` 的 advisories、bans、licenses、
sources 均通过。release build、runner build 和 Windows production tree 分别见
`out/ely-release-build-37b.log`、`out/ely-runner-build-37b.log` 和
`out/ely-windows-production-tree-37b.log`；release build 保留一个 `linker_messages` warning，
不改变构建完成事实。生产依赖树中用于门禁的禁用包名匹配计数为 `0`。

来源 manifest 见 `out/ely-source-inputs-37.json` 和 `out/ely-source-inputs-37.log`：1550 个输入、
无 vendor drift，SOURCE 表 30/30 行哈希匹配，其中 29 个为局部文件；新增 1732、删除 300 的
统计均属于 patch 记录。

release37 原生批次完整结果为 `21 passed; 5 failed`。下表中的所有报告均绑定上述
release37 binary SHA-256；批次日志按 functional、regressions、remaining、rest、settings-rest 和 visual
分组保存。

| 计划 | 证据 | release37 结果 |
| --- | --- | --- |
| resources37 | `out/ely-native-batch-functional-37.log`、`out/ely-native/resources-37/native-run/report.json` | **passed**；272 actions，被测进程自然退出、`exit_code=0` |
| general-preferences37 | 同上、`out/ely-native/general-preferences-37/native-run/report.json` | **passed**；171 actions，被测进程自然退出、`exit_code=0` |
| provider-settings37 | 同上、`out/ely-native/provider-settings-37/native-run/report.json` | **passed**；119 actions，被测进程自然退出、`exit_code=0` |
| keybindings37 | 同上、`out/ely-native/keybindings-37/native-run/report.json` | **passed**；61 actions，被测进程自然退出、`exit_code=0` |
| keybindings-cold-resume37 | 同上、`out/ely-native/keybindings-cold-resume-37/native-run/report.json` | **passed**；35 actions，被测进程自然退出、`exit_code=0` |
| goal-workflow37 | 同上、`out/ely-native/goal-workflow-37/native-run/report.json` | **passed**；68 actions，被测进程自然退出、`exit_code=0` |
| goal-lifecycle37 | 同上、`out/ely-native/goal-lifecycle-37/native-run/report.json` | **passed**；108 actions，被测进程自然退出、`exit_code=0` |
| automation37 | 同上、`out/ely-native/automation-37/native-run/report.json` | **passed**；75 actions，被测进程自然退出、`exit_code=0` |
| subagent37 | 同上、`out/ely-native/subagent-37/native-run/report.json` | **failed**；29 actions，ordinal `28` 等待本轮新增 `transcript_segment_committed` 超时；进程自然退出，runner `exit_code=1`、`processExit=0` |
| full37 | `out/ely-native-batch-remaining-37.log`、`out/ely-native/full-37/native-run/report.json` | **passed**；63 actions，被测进程自然退出、`exit_code=0` |
| cold-resume37 | 同上、`out/ely-native/cold-resume-37/native-run/report.json` | **passed**；35 actions，被测进程自然退出、`exit_code=0` |
| permission-askuser37 | 同上、`out/ely-native/permission-askuser-37/native-run/report.json` | **failed**；29 actions，ordinal `28` 等待 `turn_completed` 超时；`termination=killed-after-timeout`，runner `exit_code=1`、`processExit=1` |
| stop-reopen37 | `out/ely-native-batch-rest-37.log`、`out/ely-native/stop-reopen-37/native-run/report.json` | **passed**；53 actions，被测进程自然退出、`exit_code=0` |
| workbench37 | 同上、`out/ely-native/workbench-37/native-run/report.json` | **failed**；49 actions，ordinal `48` 保存 UTF-8 文件时 UIA 子树不可用；被测进程 `processExit=-1073740791`，报告 `exit_code=1` |
| code-settings37 | `out/ely-native-batch-regressions-37.log`、`out/ely-native/code-settings-37/native-run/report.json` | **passed**；71 actions，被测进程自然退出、`exit_code=0` |
| code-settings-cold-resume37 | 同上、`out/ely-native/code-settings-cold-resume-37/native-run/report.json` | **passed**；26 actions，被测进程自然退出、`exit_code=0` |
| navigation37 | 同上、`out/ely-native/navigation-37/native-run/report.json` | **passed**；108 actions，被测进程自然退出、`exit_code=0` |
| hooks37 | 同上、`out/ely-native/hooks-37/native-run/report.json` | **passed**；123 actions，被测进程自然退出、`exit_code=0` |
| hooks-cold-resume37 | 同上、`out/ely-native/hooks-cold-resume-37/native-run/report.json` | **passed**；37 actions，被测进程自然退出、`exit_code=0` |
| provider-model37 | 同上、`out/ely-native/provider-model-37/native-run/report.json`、`network-trace.jsonl` | **failed**；17 actions，ordinal `16` 等待 UIA 标签“模型真实生成测试通过”超时 `176369ms`。该动作是设置页独立模型生成测试，不创建会话 Journal，因此 `turn_started=0`、终态事件 `0` 属于预期；模型请求记录为 `success`、HTTP `200`，但 UIA 未观察到成功 notice；进程自然退出、`exit_code=0` |
| settings37（深色全设置） | `out/ely-native-batch-settings-rest-37.log`、`out/ely-native/settings-37/native-run/report.json` | **passed**；102 actions，被测进程自然退出、`exit_code=0` |
| settings-light37（浅色全设置） | 同上、`out/ely-native/settings-light-37/native-run/report.json` | **passed**；102 actions，被测进程自然退出、`exit_code=0` |
| font-settings37 | 同上、`out/ely-native/font-settings-37/native-run/report.json` | **passed**；72 actions，被测进程自然退出、`exit_code=0` |
| unsupported-hooks-diagnostics37 | 同上、`out/ely-native/unsupported-hooks-diagnostics-37/native-run/report.json` | **failed**；19 actions，ordinal `18` 等待 UIA 标签“当前 Native 执行入口未实现”超时；进程自然退出、`processExit=0`，报告 `exit_code=1` |
| visual-settings37 | `out/ely-native-batch-visual-37.log`、`out/ely-native/visual-settings-37/native-run/report.json` | **passed**；24 actions，进程自然退出、`exit_code=0`；像素比较仍为 `pending` |
| visual-workspace37 | 同上、`out/ely-native/visual-workspace-37/native-run/report.json` | **passed**；13 actions，进程自然退出、`exit_code=0`；像素比较仍为 `pending` |

release37 的 source manifest 和完整原生批次已有独立证据；5 个失败项必须保留其报告状态。
provider-model 的 HTTP `200` 只证明设置页独立模型测试请求有效，不能替代完整会话 UI 回合
证据。完整产品仍为 `pending`，像素比较和性能验收仍为 `pending`。

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

38c workspace tests 曾挂起，root 核实 PID `4720` 后终止进程，现场记录为
`out/ely-workspace-test-stall-38c.json`；这不改写 38g 最终门禁结果。Provider offline test 已
seed catalog，但没有新的产品门禁结论；当前观察也不能把 runtime stack 根因宣称为已 100% 证实。
editor38d 编译失败；editor38e 实际记录为 `19 passed; 1 failed`，失败为
`replacing_a_multiline_selection_invalidates_the_previous_rows`，证据见
`out/ely-vendor-editor-tests-38e.log`。editor38f 的 vendor editor 测试为 `20 passed; 0 failed`，
使用真实 GPUI 首帧和输入模拟验证两项新回归，证据见 `out/ely-vendor-editor-tests-38f.log`；这些
editor 结果只覆盖 vendor editor。

release38 的视觉计划如下；两计划均绑定上述 SHA，24/13 actions 均通过，被测进程自然退出、
`exit_code=0`：

| 计划 | 证据 | release38 结果 |
| --- | --- | --- |
| visual-settings38 | `out/ely-native-batch-visual-38.log`、`out/ely-native/visual-settings-38/native-run/report.json` | **passed**；24 actions，进程自然退出、`exit_code=0` |
| visual-workspace38 | 同上、`out/ely-native/visual-workspace-38/native-run/report.json` | **passed**；13 actions，进程自然退出、`exit_code=0` |
| General settings comparison | `out/ely-native/visual-settings-38/general-comparison/comparison-metrics.json` | `acceptance=pending`、`assessment=not_identical`；753210/4198400 个像素不同，差异率 `17.9404058689%` |
| Appearance comparison | `out/ely-native/visual-settings-38/appearance-comparison/comparison-metrics.json` | `acceptance=pending`、`assessment=not_identical`；155268/4198400 个像素不同，差异率 `3.6982660061%` |
| Workspace comparison | `out/ely-native/visual-workspace-38/draft-reference8-comparison/comparison-metrics.json` | `acceptance=pending`、`assessment=not_identical`；239063/4198400 个像素不同，差异率 `5.6941453887%` |

除上述离线门禁和两个视觉计划外，release38 的其他功能计划未运行，不能记录为 PASS；完整产品、
像素验收和性能验收仍按各自证据保持 `pending`。当前正在进行 release39 toolbar 参数调整，
因此 release38 binary 和上述证据不代表调整后的源码；release37 的失败历史保持原样。

## release36 离线门禁与原生批次检查点（2026-10-06）

release36 目标 binary 的 SHA-256 为
`14894efe6301874e91e6a2b34e9af3dfc5847fc71e62e8ea02dd7c54fcd121eb`。36b 的 fmt、fmt
check、Clippy 和 workspace tests 证据见 `out/ely-workspace-fmt-check-36b.log`、
`out/ely-workspace-clippy-36b.log`、`out/ely-workspace-tests-36b.log`；workspace tests
共有 55 groups，合计 `2660 passed; 0 failed; 7 ignored`。`out/ely-workspace-doctests-36b.log`
中的 doctests 也已完成。36c 使用 `out/tools/cargo-deny/bin` 下的 cargo-deny `0.20.2`，
`out/ely-cargo-deny-36c.log` 记录 advisories、bans、licenses、sources 均通过；release
和 runner build 分别见 `out/ely-release-build-36c.log`、`out/ely-runner-build-36c.log`，
`out/ely-windows-production-tree-36c.log` 的禁用包名计数为 `0`。

`out/ely-source-inputs-36.json` 记录 1544 个 source manifest 输入，`out/ely-source-inputs-36.log`
记录 vendor drift 未发现；SOURCE 表为 30 行，其中 29 行为 patch。上述内容只证明 release36
门禁和来源审计，不提升完整产品或原生交互验收状态。

release36 的原生批次结果如下：

| 计划 | 证据 | release36 结果 |
| --- | --- | --- |
| Navigation36 | `out/ely-native-batch-functional-A-36.log`、`out/ely-native/navigation-36/native-run/report.json` | **failed**；ordinal `46` 在前进 click 后等待 UIA 标签“任务通知”超时；被测进程自然退出、`exit_code=0` |
| Hooks36 | `out/ely-native-batch-functional-B-36.log`、`out/ely-native/hooks-36/native-run/report.json` | **passed**；123 actions，被测进程自然退出、`exit_code=0` |
| Hooks cold-resume36 | `out/ely-native-batch-functional-B-36.log`、`out/ely-native/hooks-cold-resume-36/native-run/report.json` | **failed**；34 actions，ordinal `33` 在删除 Hook 后断言 trust 仍非空 |
| functional-C36 / provider-model | `out/ely-native-batch-functional-C-36.log`、`out/ely-native/provider-model-36/native-run/report.json`、`network-trace.jsonl` | **failed**；ordinal `16` 等待“模型真实生成测试通过”超时；真实网络已收到 HTTP `200` 和 `MessageEnd`，但 complete 可能仍等待 EOF，不能判定通过 |
| functional-D36 / provider-settings | `out/ely-native-batch-functional-D-36.log`、`out/ely-native/provider-settings-36/native-run/report.json` | **passed**；119 actions，被测进程自然退出、`exit_code=0` |
| functional-D36 / code-settings | `out/ely-native-batch-functional-D-36.log`、`out/ely-native/code-settings-36/native-run/report.json` | **failed**；44 actions，ordinal `43` 的 `Space` 持久化为 `true` 断言失败，流程尚未走到 `Enter` |
| functional-E36 / keybindings | `out/ely-native-batch-functional-E-36.log`、`out/ely-native/keybindings-36/native-run/report.json` | **passed**；61 actions，被测进程自然退出、`exit_code=0` |
| functional-E36 / keybindings cold-resume | `out/ely-native-batch-functional-E-36.log`、`out/ely-native/keybindings-cold-resume-36/native-run/report.json` | **passed**；35 actions，被测进程自然退出、`exit_code=0` |
| Visual settings/workspace36 | `out/ely-native-batch-visual-36.log`、`out/ely-native/visual-settings-36/native-run/report.json`、`out/ely-native/visual-workspace-36/native-run/report.json` | 取证均通过；分别为 24/13 actions、自然退出 `0`，像素比较仍为 `pending` |

release36 的视觉比较、完整产品和性能验收仍保持 `pending`；后续 release37 的代码修复不在
本检查点内，不能写成 PASS。release35 及更早章节保留各自 binary、报告和历史判断。

## release35 离线门禁与原生批次验收（2026-10-06）

release35 目标 binary 的 SHA-256 为
`e66a3872a2219821722dbb0e6b6facc4b65c05f2e883c17f10aa43949efb506d`，对应证据见
`out/ely-release35-sha256.txt`。`out/ely-workspace-tests-35b.log` 含 55 个
`test result:`，合计 `2659 passed; 0 failed; 7 ignored`。`out/ely-release-gates35b.log`
只记录末段 vendor/native 测试的 `28 passed; 0 failed`，不能替代完整 workspace 汇总，
也不应与完整汇总相加。该日志同时记录 release 构建完成，但保留一个
`linker_messages` warning；这不改变构建事实，也不能扩展为功能批次通过。

`out/ely-windows-production-tree-35b.log` 是 `keencode-desktop v0.0.1` 的生产依赖树
记录；目标外的 Tauri、Wry、WebView2、Electron、Node.js 和 JavaScript engine 关键字
未命中。该记录只证明生产依赖树审计结果，不证明完整产品或原生交互验收通过。

release35 的导航和功能批次结果如下：

| 计划 | 证据 | release35 结果 |
| --- | --- | --- |
| Navigation | `out/ely-native/navigation-35/native-run/report.json`、`process-output.json`、`accessibility/0046-journal-timeout-uia-target.json`、`screenshots/0046-journal-timeout-uia-target.bmp` | **failed**；在 ordinal `46` 等待 UIA 标签“任务通知”超时，报告错误为 `等待 UIA 标签超时：任务通知`；被测进程自然退出、`exit_code=0`，诊断证据已保留 |
| functional-B35 / hooks | `out/ely-native-batch-functional-B-35.log`、`out/ely-native/hooks-35/native-run/report.json` | **failed**；ordinal `70` 等待 `hooks-user-marker.txt` 超时；被测进程自然退出、`exit_code=0` |
| functional-C35 / provider-model | `out/ely-native-batch-functional-C-35.log`、`out/ely-native/provider-model-35/native-run/report.json` | **failed**；ordinal `13` 断言缺少 JSON Pointer `/providers/0/disabledModels` |
| functional-C35b / provider-model 重跑 | `out/ely-native-batch-functional-C-35b.log`、`out/ely-native/provider-model-35b/native-run/report.json` | **failed**；ordinal `13` 的 `/providers/0/models` JSON Pointer 不匹配。隔离 `providers.json` 的源 provider 实际有 4 个模型，而计划按 1 个模型断言；这是验收计划/fixture 假设不匹配，不能据此判定业务功能错误，需修订计划后复测 |
| functional-D35 / provider-settings | `out/ely-native-batch-functional-D-35.log`、`out/ely-native/provider-settings-35/native-run/report.json` | **failed**；ordinal `45` 等待 UIA 标签“当前模型”超时；被测进程自然退出、`exit_code=0` |
| functional-E35 | `out/ely-native-batch-functional-E-35.log` 及 resources、general-preferences、code-settings、code-settings-cold-resume、keybindings 报告 | **failed**；resources `272`、general-preferences `171`、code-settings `63`、cold-resume `26` actions 通过；keybindings 在 ordinal `32` 断言目标文件未包含要求文本 `native-keybindings.json`，目标文件实际存在，属于内容断言不匹配 |

Navigation 报告中的进程自然退出只说明退出方式，不能把 UIA 超时改写为导航功能通过。当前
各功能批次的结果均已按各自报告记录；单个批次的通过动作数不能抵销其整体 `failed`
状态。C35b 的失败应先修订计划断言，再重新绑定同一 release35 binary 复测。

`out/ely-native-batch-visual-35.log` 中 `visual-settings` 通过、首次
`visual-workspace` 失败；`out/ely-native-batch-visual-workspace-35b.log` 的重跑通过。
这些记录只提供当前视觉取证，尚未形成同主题、同窗口、同 DPI、同字体和同页面状态的像素
比较结论。

产品裁剪边界保持不变：界面语言切换仍属于 native 验收中的未完成项，`N-17` 保持
`pending`；Office 模式和主动任务推荐已按产品裁剪，兼容记录固定为 `interfaceMode=coding`，
不把这两个缺失控件计入原生功能失败或像素对齐要求。

release35 的离线门禁、生产依赖树、原生批次和 navigation 诊断均不能提升完整版产品验收状态。
像素验收仍须绑定同主题、同窗口、同 DPI、同字体和同页面状态的当前目标截图与比较报告；
完整版、像素和性能仍保持 `pending`。release33 及更早证据也不覆盖 release35 或当前
工作树的未验证范围。

## release33 原生验收（2026-10-06）

release33 的所有下列报告均绑定同一个 release binary，SHA-256 为
`b3841d8a26e286110396bb6bae43fdb5985f5b36fde44b03391de7fd0d592215`。构建证据为
`out/ely-release-build-33d.log`、`out/ely-runner-build-33d.log` 和
`out/ely-release33-sha256.txt`。这些结果只描述该 binary 的运行，不覆盖后续
release34 源码；release34 已有 Hooks 补齐和旧 benchmark 清理等后续变化。

| 计划 | 证据 | release33 结果 |
| --- | --- | --- |
| Provider settings | `out/ely-native/provider-settings-33/native-run/report.json` | PASS，119 actions，进程自然退出，exit code `0` |
| Visual settings | `out/ely-native/visual-settings-33/native-run/report.json` | PASS，24 actions，4 份截图、3 份 UIA、4 份 metrics；GPU 采样仍为 `pending` |
| Code settings | `out/ely-native/code-settings-33/native-run/report.json` | PASS，63 actions，进程自然退出，exit code `0` |
| Code settings cold resume | `out/ely-native/code-settings-cold-resume-33/native-run/report.json` | PASS，26 actions，进程自然退出，exit code `0` |
| Goal workflow | `out/ely-native/goal-workflow-33/native-run/report.json` | PASS，68 actions；新增 `turn_started=1`、终态事件 `1`，自然退出 |
| Goal lifecycle | `out/ely-native/goal-lifecycle-33/native-run/report.json` | PASS，108 actions；新增 `turn_started=2`、终态事件 `2`，自然退出 |
| Automation | `out/ely-native/automation-33/native-run/report.json` | PASS，75 actions；新增 `turn_started=2`、终态事件 `2`，自然退出 |
| Resources | `out/ely-native/resources-33/native-run/report.json` | failed；257 个动作均为 passed，但没有新增 Turn，报告因缺少真实 TurnStarted 与终态事件失败 |
| General preferences | `out/ely-native/general-preferences-33/native-run/report.json` | failed；51 actions 中 50 个通过，1 个 UIA click 因 bounds 与 client 无交集失败，进程自然退出 |
| Keybindings | `out/ely-native/keybindings-33/native-run/report.json` | failed；56 actions 中 55 个通过，要求 `turn_started=1`、实际观察到 `2`，最终 `killed-after-timeout` |
| Subagent（附加运行） | `out/ely-native/subagent-33/native-run/report.json` | failed；20 actions 中 19 个通过，等待新增 `turn_started` 超时；该报告的被测进程自然退出，但 runner 结果仍失败 |

release33 的离线门禁记录为：`out/ely-workspace-tests-33d.log` 共有
2636 passed、0 failed、7 ignored、55 groups；`out/ely-workspace-clippy-33d.log`
完成 Clippy/编译检查；`out/ely-vendor-choices-tests-33e.log` 共有 17 passed。
这些门禁同样只绑定 release33，不能代表当前 release34 源码树。

release33 的 `out/ely-native-batch-functional-33.log` 只保留批次摘要，功能判定以各
`report.json` 为准。Resources、General、Keybindings 和 Subagent 的失败报告必须保留，
不能因动作大部分通过、窗口正常退出或已有截图而改写为产品通过。

## release32 原生验收（2026-10-06）

release32 已完成 release 构建，报告绑定的 binary SHA-256 为
`95f3778547682d46412345a21d8261c0920512d934f036ae6d6e2d8db26ccee7`；证据见
`out/ely-release-build-32.log`、`out/ely-release32-sha256.txt` 和各计划的
`out/ely-native/*-32/native-run/report.json`。`goal-workflow-32` 的 68 个动作通过，
`goal-lifecycle-32` 的 108 个动作通过；两者均自然退出，Journal 分别记录
`turn_started=1/2` 和终态事件 `1/2`。

`automation-32` 的 75 个计划动作均记录为 passed，但报告整体为 `failed`：收尾发送
Ctrl+Q 时 `SetForegroundWindow` 返回 Win32 error 1400；本轮 Journal 与网络 trace
仍已记录两轮 Turn 和 10704 bytes trace。`provider-settings-32`、`resources-32`、
`general-preferences-32` 和 `keybindings-32` 均为 `failed`，分别出现 UIA 标签等待、
资源标题等待、控件 bounds 越界和 `turn_started` 等待超时，等待 release33 复测。

`visual-settings-32` 虽生成了 24 个动作的取证报告，视觉像素、完整视觉产品和性能
验收仍保持 `pending`；GPU 证据也仍为 `pending`。`out/ely-workspace-tests-32d.log`
的 2634 passed、`out/ely-workspace-clippy-32f.log` 的通过记录以及 runner/vendor
离线测试，只能作为 release32 构建门禁。release32 之后已有源码变更，不能把这些记录
沿用为当前工作树的新版本验收结论。

## 最新检查点：release31（2026-10-06）

被测 binary SHA-256 为 `1e59a14e260a5997c0150a3332f56275e12945ae9b9671c4b68027f53d7061c0`。
`workbench-31` 的 107 个动作通过，覆盖原生文件编辑、PTY、Git/worktree 和搜索；
`goal-workflow-31` 的 68 个动作通过，覆盖真实模型 Workflow/Goal 执行。
两项均自然退出、exit code 为 0。`visual-settings-31` 的 24 个截图取证动作通过，
只证明取证完成，不能作为像素一致性结论。

Provider 创建和模型配置已落盘，但 `provider-settings-31` 在编辑阶段因两个
“供应商名称”可点击 UIA 节点失败。`resources-31` 已保存 MCP 配置，但等待资源标题
UIA 名称超时。`goal-lifecycle-31` 在 blocked reason 的 JSON Pointer 断言处失败。
这些失败报告保留原状；后续源码修复必须重新构建和绑定新 SHA 验证。

`automation-31` 在 ordinal40 等待 `tool_requested` 超时，未调用到 AskUser。
已定位到 Automation 缺少原生 elicitation connection 绑定；开发修复尚待新版原生复测。

当前正在开发 schema v2 代码外观设置、完整编辑器换行、General 偏好和设置控件像素校正。
release31 不覆盖这些新源码，完整产品、像素和性能仍为 `pending`。

## 历史原生结果：release30（2026-10-06）

被测 release30 SHA-256 为 `4e83c5fa2e117b63d11e2312be386c57095b603d9a7f9fd0e22549d5cf95b784`，runner 使用
`target/debug/keencode-native-gpui-tests.exe`。`goal-workflow-30b` 的正式计划共
68 actions 全部通过，真实 actor 请求、产物权限批准、artifact-committed 和
run-settled(succeeded) 均有 Journal 强断言；进程自然退出且 exit code 为 0。
报告为 `out/ely-native/goal-workflow-30b/native-run/report.json`，脱敏 trace 2928 bytes。

Provider30b 已由真实错误截图确认缺少初始模型视觉能力配置；workbench30 的失败位于
worktree 数量断言；这两项生产修复尚未绑定新 binary 复测。

`subagent-30c` 的正式计划 48 actions 全部通过，单层 child 执行 Read，父子标记和
spawn/wait 均有真实 Journal 强断言；turn_started=2、终态=2，脱敏 trace 16838 bytes，
自然退出且 exit code 为 0。30 的输入定位失败、30b 的字段断言失败报告继续保留，
不能修改历史报告状态；此通过项仍只绑定 release30。

Appearance、Terminal、Goal 生命周期和资源页分组仍在开发，当前未达到完整版验收。
以下旧批次保留用于追溯，不能作为新 binary 的通过结果。

## 历史运行说明

## 当前结论

`keencode-native-gpui-tests` 是一个纯 Rust Windows 验收器。它通过 Win32
`EnumWindows` 找到被测进程的真实顶层 `HWND`，用 `SendInput` 注入键鼠，用
`PrintWindow` 生成原生 BMP，并用 Windows UI Automation 读取窗口根节点属性和
子窗口边界。它不加载 CDP、JavaScript、浏览器脚本或 WebView，也不通过 mock、
domain/headless runner 或测试 mailbox 代替用户操作。

此前的真实 Provider 原生证据 `smoke-15`：报告的 `status=passed`，计划
`ely-release18-live-ui-smoke` 同时声明
`requires_real_provider`、`requires_journal` 和 `requires_network_trace`，并使用隔离配置
`out/native-live/native-provider-6f2.json`（只记录路径）。被测 binary 为
`target/release/keencode-desktop.exe`，SHA-256 为
`6649d8d173f56213e591853719829fc9a3541a3d8452f796da31124a00e1cade`；实际 Provider 摘要为
`native-live-deepseek` / `deepseek-v4.1-flash`，真实 Provider 已授权。隔离 Journal 有
1 个新增 `turn_started` 和 1 个终态 `turn_completed`，`events.jsonl` 为 4832 bytes；
脱敏 `network-trace.jsonl` 为 5466 bytes，stream `chat_completions` 请求记录返回 HTTP 200，
模型请求记录包含真实输入边界的 token 摘要（不落盘认证 Header、Key 或凭据 URL）。
`0008-typed.bmp`、`0011-started.bmp`、`0014-completed.bmp` 和
`0015-completed.json` 均已生成，进程自然退出且 exit code 为 0。该结果证明最小真实
Provider 回合链已跑通；完成产品项仍为 `pending`，因为完成截图中 Composer 输入未清空并出现
`hostdraft error`，且没有足够证据把它提升为完整 UI 验收通过。

同一 binary 的 `smoke-15-diagnostic` 也为 `passed`，计划为
`ely-release17-live-diagnostics`，Journal 为 4843 bytes、`turn_started=1`、终态事件为 1，
脱敏 network trace 为 5474 bytes，截图为 `0008-typed.bmp`、`0011-started.bmp`、
`0013-completed.bmp`，metrics 为 `0014-completed.json`。两次运行的 Provider/model 摘要和
真实输入边界均来自运行时 Journal 与 trace，不以配置加载或授权事件代替模型请求。

`smoke-20` 是后续的失败证据，不应写成通过：计划为 `ely-release20-live-ui-smoke`，绑定 binary SHA-256
`868fc686fcd213ece93811a8787a6af0e40513f6e01c9fa3c298d2badee17967`，输入截图
`0009-typed.bmp` 已显示前缀丢字；Enter 后没有新增 `turn_started`，报告因 Journal 等待超时
为 `failed`，network trace 与 `model-request-records.jsonl` 均为 0 bytes。因此真实 prompt、
完整回合、像素和性能产品项继续保持 `pending`。

随后 `smoke-22` 仍为失败证据：绑定 release22 binary SHA-256
`17668ba0f3fc2eb25f02aae5b791f4d5e83b41fb79822d105f26cafcb3b8a19a`，报告中的 click/type/key
action 虽显示 `passed`，但输入未命中，没有新增 `turn_started`，最终因等待 Journal 超时；
`screenshots/0011-typed.bmp` 与诊断图 `input-crop.png` 是主要证据，network trace 和
`model-request-records.jsonl` 均为 `0` bytes，进程自然退出且 `exit_code=0`。它不能视为真实
输入提交成功。

同一 release22 binary 的 `smoke-22b` 报告为 `status=passed`，计划为
`out/ely-release22-live-ui-plan.json`：真实 Journal 有 `turn_started=1`、`terminal_event=1` 和
`turn_completed=1`，并记录真实用户 prompt 与 assistant `native smoke ok`；network trace
为 `5466` bytes，model request records 为 `3145` bytes。截图 `0011-typed.bmp`、
`0014-started.bmp`、`0017-completed.bmp` 和 `metrics/0018-completed.json` 均生成，隔离
draft `native-drafts/e4d422832dda039d3663db30fdad84ccb7de1bb1a22a29c4476731c76dbe6156.json`
的 `text` 为空，且用户确认完成截图正文可见；进程自然退出、`exit_code=0`。这只证明真实
模型请求、Enter 提交、Journal 回合和 draft 清空链通过，完整产品功能、像素和性能仍为
`pending`，metrics 的 GPU 状态仍为 `pending`。

release24 binary SHA-256 为
`56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f`。`smoke-24` 报告为
`status=passed`，计划为 `out/ely-release24-live-ui-plan.json`；计划和动作记录覆盖真实
Shift+Enter 预填、Enter 提交及空白 hitarea，`turn_started=1`、`terminal_event=1`、
`turn_completed=1`，network trace 为 `5466` bytes，model request records 为 `3145` bytes。
`0015-typed.bmp`、`0018-started.bmp`、`0021-completed.bmp` 和 `metrics/0022-completed.json`
均生成，完成截图正文可见且 Composer 清空。报告列出本轮 draft 文件，但运行未保留
isolation，因此不把 draft 的精确持久化正文作为独立证据；该结果只证明最小真实请求、提交、
Journal 和清稿链，完整产品、像素和性能仍为 `pending`，GPU 仍为 `pending`。

`full-24` 使用同一 release24 binary，Read/Write 的真实工具事件和项目 marker
`KEENCODE_NATIVE_ACCEPTANCE_MARKER_20261005` 均完成，但报告因等待 `turn_completed` 超时
而 `failed`：`turn_started=1`、`terminal_event=0`，进程 `exit_code=1`，
`termination=killed-after-timeout`。运行时只读诊断表明 `collect_model_stream` 已返回，
且 `execute_tools` 的审批路径先于对应的 `tool_requested`；Journal 只记录了已落盘的工具事件，
没有记录具体第三个 tool，因此无法确定该 tool 的实际审批结果。失败时报告只生成了
`accessibility/0005-startup.json` 和 `accessibility/0012-session-created.json`，两者的 UIA
helper、provider 和 subtree 均可用；`0015-permission-options.bmp`、
`0018-automatic-edit-selected.bmp` 和 `0025-turn-started.bmp` 只有截图，未生成失败阶段的
post-failure UIA 树或 `turn-completed` 取证。该运行保留工具链诊断证据，不能提升完整产品验收。

`stop-reopen-24c` 是当前最新的停止/重开原生证据：报告 `status=passed`，计划为
`tooling/native-gpui-tests/ely-native-stop-reopen.json`，绑定 release24 binary SHA-256
`56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f`。Journal 有
`turn_started=2`、`terminal_event=2`，其中实际 `turn_stopped=1` 且 payload 的 `reason=cancelled`，
第二轮有 `turn_completed=1`；`events.jsonl` 为 18354 bytes，脱敏 network trace 为 13850 bytes，
进程自然退出且 `exit_code=0`。`0025-before-stop.bmp`、`0030-after-stop.bmp`、
`0036-session-reopened.bmp` 和 `0050-second-round-completed.bmp`，以及对应的 UIA/metrics
证据均已生成。这只证明停止、会话重开和第二轮回合链通过，不提升未覆盖的产品、像素或性能项。

`settings-dark-24` 初次运行因真实验收缺少非空 Session Journal 失败，Journal 与 network
trace 均为 `0` bytes。修复后的 `settings-dark-24b` 报告为 `passed`，General、Providers、
Resources、Automations、Workflows、Agents、Usage、Diagnostics 八个设置页均生成截图和
UIA 证据，并有 `turn_started=1`、`terminal_event=1`、`turn_completed=1` 及 `5466` bytes
network trace；它只证明设置页和最小真实回合链，完整产品、像素和性能仍为 `pending`。

release25 binary SHA-256 为
`2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452`。`full-25` 报告为
`status=passed`，计划为 `tooling/native-gpui-tests/ely-native-full.json`；隔离 Journal
记录 `turn_started=2`、`terminal_event=2`、`turn_completed=2`，并记录 3 次
`tool_requested`、`tool_execution_started`、`tool_completed`，其中包含 Read 和两次 Write，
两次写入 marker 分别为 `KEENCODE_NATIVE_ACCEPTANCE_MARKER_20261005` 和
`KEENCODE_NATIVE_ACCEPTANCE_SECOND_MARKER_20261005`。本轮 Journal 事件文件合计 `25847`
bytes，脱敏 network trace 为 `19638` bytes，model request records 为 `11180` bytes；
截图、UIA 和 metrics 分别生成 9、6、6 份，包含 `0039-session-reopened.bmp`、
`0053-reopened-send-completed.bmp` 和 `0060-second-session-created.bmp`。进程自然退出且
`exit_code=0`。这证明 release25 的工具、会话重开和第二轮回合链，完整产品、像素和性能仍为
`pending`。

`settings-dark-25` 与 `settings-light-25` 均绑定上述 release25 binary，报告均为
`status=passed`，计划分别为 `tooling/native-gpui-tests/ely-native-settings.json` 和
`tooling/native-gpui-tests/ely-native-settings-light.json`。深色和浅色两轮均覆盖 General、
Providers、Resources、Automations、Workflows、Agents、Usage、Diagnostics 八个设置页，
各有 9 份截图、9 份 UIA 和 2 份 metrics，并有 `turn_started=1`、`terminal_event=1`、
`turn_completed=1`、`5466` bytes network trace、`3145` bytes model request records；进程
均自然退出且 `exit_code=0`。这只证明 release25 的深浅设置页和最小真实回合链，不能提升全部
产品、像素或性能项。

`release25-calibration` 报告为 `status=passed`，计划为 `out/ely-release25-calibration.json`，
绑定同一 release25 binary；窗口和截图/metrics 链通过，截图为 `0005-startup.bmp`，metrics
为 `0006-startup.json`。与固定 reference6 的 `2560x1640` client 比较有 `314065` 个不同
像素，差异比例为 `7.4805878%`，比较文件仍记录 `acceptance=pending`、
`assessment=not_identical`，因此不能视为像素通过。

`permission-askuser-25` 绑定同一 release25 binary，报告为 `failed`：UIA helper 使用默认
DPI 导致 `click_accessibility` 坐标偏移，后续等待“自动编辑”标签超时；该失败属于 runner
取证/坐标问题，Journal 没有新增 Turn，不能解释为产品 Permission 功能失败或通过。

上述新增结果均属于 release25 SHA-256 `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452`，不代表 release26，也不代表像素验收通过。

release26 当前被测 binary SHA-256 为
`2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94`。
`font-settings-26` 是首次 runner 失败：计划使用普通坐标 click，点击后状态未稳定，最后
`assert_file` 未找到 `native-general-settings.json`；它不能作为字体功能结论。
修正后的 `font-settings-26b` 报告为 `status=passed`，计划为
`tooling/native-gpui-tests/ely-native-font-settings.json`，通过 UIA 定位并验证输入
`Consolas`、应用、离开后重开设置页、清空输入并应用系统默认回退；最终隔离文件
`native-general-settings.json` 的 `fontFamily` 为 `系统默认`，7 份截图、4 份 UIA、进程
自然退出且 `exit_code=0`。这只证明字体设置持久化/回退链，不提升像素或完整产品验收。

`permission-askuser-26c` 绑定同一 release26 binary，报告为 `failed`。它已启动真实 Turn，
但没有终态事件，脱敏 network trace 为 `0` bytes；`0021-permission-pending.bmp` 与对应
UIA 树显示长 JSON 工具消息把聊天内容撑宽，`允许`/`拒绝` UIA bounds 落在窗口 client
之外，后续 `click_accessibility` 报告 bounds 与 client 无交集并超时。该运行暴露的是
`gpui_chat` 的长 JSON 布局/权限按钮 offscreen 产品问题，审批结果不能从本轮报告推出。

`workbench-26` 绑定同一 release26 binary，报告为 `failed`：进入工作台并切换终端后，
等待“新建终端”的 UIA 标签超时（exact=0），未形成 Journal 或网络证据；进程虽自然退出、
`exit_code=0`，不构成工作台通过。fixture Git 同时向父仓库解析出过大 diff，
`native_workbench` 仍在排查，不能把本轮写成产品或 Git 功能通过。

`release26-calibration` 也绑定上述 release26 binary，报告为 `status=passed`；窗口和截图/metrics
链通过，与固定 reference6 的 `2560x1640` client 有 `314065` 个不同像素，差异比例为
`7.4805878%`，比较文件仍记录 `acceptance=pending`、`assessment=not_identical`。它只构成
视觉校准证据，不提升像素、产品或性能验收状态。

上述 release26 功能记录只描述各自的设置、权限和工作台范围；结合视觉校准也没有完整产品
或像素 `PASS`。

release27 当前被测 binary SHA-256 为
`3d4412d7937f80b23ef36699594483c073b9c0cef250cc7da24c34eee3f1649c`。
`permission-askuser-27` 报告为 `status=passed`，计划为
`tooling/native-gpui-tests/ely-native-permission-askuser.json`；两轮 Turn 均形成
`turn_started=2`、`tool_requested=2`、`turn_completed=2`，network trace 为 `14154` bytes，
Journal 事件文件为 `19364` bytes，model request records 为 `8103` bytes。第一轮 UIA/Journal
记录的精确消息为“请调用 Bash 工具执行 printf 'KEENCODE_PERMISSION_MARKER_20261005' >
permission-marker.txt，然后简短回答。”；`允许` 后 `permission-marker.txt` 已生成，文件为
35 bytes，内容严格为 `KEENCODE_PERMISSION_MARKER_20261005` 且无换行。第二轮 AskUser 的
精确问题为“请选择原生验收选项（可多选）。”，选项严格为 `A`、`B`，`multiSelect=true`、
`allowCustom=false`，选择 `A` 并提交后形成终态。运行生成 5 份截图、5 份 UIA 和 1 份 metrics，
进程自然退出且 `exit_code=0`。这只证明该 Permission/AskUser 范围，不能提升像素或完整产品验收。

`workbench-27` 绑定上述 release27 binary，报告为 `failed`：工作台、终端和“新建终端”
UIA 定位以及终端创建截图通过，随后输入/按键动作完成，但等待项目输出
`terminal-marker.txt` 超时；仅生成 2 份截图、2 份 UIA 和 1 份 metrics，Journal 事件与
network trace 均为 `0` bytes，进程自然退出且 `exit_code=0`。`workbench-27b` 是同一 binary
的复测，已进一步生成终端创建、命令输入截图和对应 UIA，仍在等待
`terminal-marker.txt` 时超时；生成 3 份截图、3 份 UIA 和 1 份 metrics，Journal 与 network
trace 仍为 `0` bytes。两轮均不能写成工作台或 Git 功能通过，终端高度相关根因仍待后续复测。

上述 release26/release27 功能记录只描述局部设置、权限/AskUser 和工作台范围；结合现有
视觉校准也没有完整产品或像素 `PASS`。

release28 当前被测 binary SHA-256 为
`a902bc39845758ecab43fc2d6a415877de277b97226edd7be19979649cb09e1b`。
`workbench-28` 仍为 `failed`：工作台、终端创建、真实终端高度、UIA 定位和终端输入动作均已
完成，随后等待 `terminal-marker.txt` 超时。报告保存了 4 份截图、4 份 UIA 和 1 份 metrics；
Journal 与 network trace 均为 `0` bytes，进程自然退出且 `exit_code=0`。当前根因记录为 CMD
未接受带 `\\?\` 前缀的盘符 cwd，启动回退到 `C:\Windows`，所以 marker 和 `.git` 写入没有落在
预期隔离项目目录；该运行仍不能写成 Workbench/Git 通过。报告位于
`out/ely-native/workbench-28/native-run/report.json`。

`goal-workflow-28` 也为 `failed`：本轮使用隔离 Provider 配置
`out/native-live/native-provider-6f2.json`（仅记录路径），Goal JSON 已写入
`out/ely-native/goal-workflow-28/native-run/isolation/data/goals/session-goal-f46d467d6ac501b92052e1e84ef9ba131c727db95852bccf78890a929535d0fc.json`，
但点击 UIA 标签“执行”失败，错误为 `exact=0 invalidBounds=0 nodesTruncated=false`；报告保存了
8 份截图和 8 份 UIA，Journal 为 `373` bytes，network trace 为 `0` bytes，进程自然退出且
`exit_code=0`，没有新的 Workflow 文件落盘。根因记录为 scope Combobox 缺少 `.selected`，
导致默认 `global` 被清空；当前 release28 报告仍是失败证据，后续修复需要重新绑定 binary
并复测，不能由已落盘 Goal 文件推导 Workflow 通过。报告位于
`out/ely-native/goal-workflow-28/native-run/report.json`。

`release28-calibration` 报告为 `status=passed`，生成 2 份截图和 2 份 metrics，进程自然退出、
`exit_code=0`；它只完成窗口/截图校准。与固定 reference6 的视觉比较仍为
`acceptance=pending`、`assessment=not_identical`，不能提升像素或完整产品验收。比较证据位于
`out/ely-native/release28-calibration/visual-comparison/comparison-metrics.json`。

最新 Ely GPUI Components 来源已拉取，当前依赖固定在
`718617939e1c4f938f21751f1fdbfe2ec230188f`（许可证和来源见
`THIRD_PARTY_NOTICES.md`）。最新 UI 代码尚未完成编译或原生验收；上述 release24 报告不能
被解释为最新 UI 已通过。

`session-calibration-12` 及 calibration13–18 是较早的窗口/UIA 校准历史：calibration12
曾因 `gpui::app::entity_map::double_lease_panic` 崩溃，后续校准虽消除了该现象，但分别在
UIA 或关闭等待阶段超时，均没有真实 Turn。`session-calibration-21` 仍为 `failed`，绑定
SHA-256 `96707a2509bdf2f6ae9362c48f5583aa88b1afaff8c95d61b0693f9615088b84`，只有
`session_created`、窗口截图和 metrics，Journal 无 Turn，network trace 为 0 bytes。
`release20-calibration` 的报告为 `passed`，但计划不要求真实 Provider、Journal 或 trace，
所以只证明该 binary 的窗口、截图、metrics 和自然退出链路，不能提升产品验收状态。

`smoke-1` 中操作 ID 含 `:` 导致 root prep 在 Provider 请求前失败的结论仍作为历史记录保留；
后续 `smoke-15` 已用新的 binary 证明真实 Turn ID 可以进入 Provider 回合链。工具自身检查
通过也不能单独推导产品原生窗口或完整 Provider 流式功能已经通过。

`zcode-calibration-6` 已验证当前 runner 的原生窗口链路：`focus`、`assert_window`、
`wait`、`screenshot` 和 `metrics` 全部通过，窗口 client 为 `2560x1640`、DPI 为 `192`，
Ctrl+Q 后被测进程以 `exitCode=0` 自然退出。该报告绑定 binary SHA-256
`96707a2509bdf2f6ae9362c48f5583aa88b1afaff8c95d61b0693f9615088b84`，证据位于
`out/ely-native/zcode-calibration-6/native-run/report.json`；它只证明窗口、真实输入、截图、
尺寸和关闭链路，不证明 Provider 或产品功能通过。

## 离线检查

`out/ely-workspace-tests-23.log` 中各测试组均为 `test result: ok`，包括
`599 passed; 0 failed; 4 ignored`、`214 passed; 0 failed; 1 ignored` 和
`224 passed; 0 failed`；`out/ely-workspace-clippy-23b.log` 完成 `keencode-desktop`
编译并记录 `Finished dev profile [unoptimized + debuginfo] target(s) in 6.00s`。
这些只证明离线测试与 Clippy 检查通过，不提升完整产品、像素或性能验收状态。

`out/ely-workspace-tests-24.log` 初次记录为 `598 passed; 1 failed; 4 ignored`，失败为
`app_exit::tests::idle_host_starts_cleanup_without_confirmation` 的固定 20ms 调度假设；
根因已修复，待重测。`out/ely-workspace-clippy-24.log` 初次命中 `manual_strip` 与
`type_complexity`；修复后的 `out/ely-workspace-clippy-24b.log` 完成 `keencode-desktop`
编译，`Finished dev profile [unoptimized + debuginfo] target(s) in 5.60s`。

Native runner 测试初次 `out/ely-runner-tests-25.log` 为 `23 passed; 1 failed`，失败原因是
JSONL 测试夹具缺少换行；修复后的 `out/ely-runner-tests-25b.log` 为 `24 passed; 0 failed`。
这些离线记录不提升完整产品或像素验收状态。

| 运行 | 报告/证据 | 判定 |
| --- | --- | --- |
| `session-calibration-13` | `out/ely-native/session-calibration-13/native-run/`；startup log、`0006-session-created.bmp`、`0007-session-created.json`、Session Journal | failed；UIA 卡死，未生成 `report.json` |
| `session-calibration-14` | `out/ely-native/session-calibration-14/native-run/report.json`；session、截图、metrics；Journal 无 Turn，trace 0 bytes | failed；5 秒超时后 runner 终止 |
| `session-calibration-15` | `out/ely-native/session-calibration-15/native-run/report.json`；session、截图、metrics；Journal 无 Turn，trace 0 bytes | failed；5 秒超时后 runner 终止 |
| `session-calibration-16` | `out/ely-native/session-calibration-16/native-run/report.json`；session、截图、metrics；Journal 无 Turn，trace 0 bytes | failed；5 秒超时后 runner 终止 |
| `session-calibration-17` | `out/ely-native/session-calibration-17/native-run/report.json`、`process-output.json`；`termination=killed-after-timeout` | failed；关闭等待超时后 runner 强制终止，Journal 无 Turn，trace 0 bytes |
| `session-calibration-18` | `out/ely-native/session-calibration-18/native-run/report.json`、`accessibility/0008-session-created.json`；helper `lastStage=uia-find:start`、`timeoutMs=5000` | failed；UIA subtree unavailable，随后关闭等待超时并记录 `termination=killed-after-timeout` |
| `session-calibration-21` | `out/ely-native/session-calibration-21/native-run/report.json`；binary SHA-256 `96707a2509bdf2f6ae9362c48f5583aa88b1afaff8c95d61b0693f9615088b84`；窗口截图、metrics、Session Journal | failed；仅有 `session_created`，无本轮 `turn_started`/终态事件，network trace 为 0 bytes |
| `smoke-1` | `out/ely-native/smoke-1/native-run/report.json`、`isolation/data/projects/*/collaboration-v2.json`、`keencode-desktop.log` | failed；Turn ID 字符校验在 Provider 请求前拒绝 |
| `release15` | `out/ely-release-build-15.log`；calibration16 report 绑定 binary SHA-256 `f90374f38069df3cf598ccd05387dc321a5ac516502feef64a93913cad8f4641` | PASS（release build 用时 1m04s；不是产品验收） |
| `release20-calibration` | `out/ely-native/release20-calibration/native-run/report.json`；binary SHA-256 `868fc686fcd213ece93811a8787a6af0e40513f6e01c9fa3c298d2badee17967`；窗口截图、metrics、自然退出 | PASS（窗口/截图/metrics 校准；计划不要求真实 Provider、Journal 或 trace，不是产品验收） |
| `zcode-calibration-6` | `out/ely-native/zcode-calibration-6/native-run/report.json`；binary SHA-256 `96707a2509bdf2f6ae9362c48f5583aa88b1afaff8c95d61b0693f9615088b84` | PASS（focus、窗口尺寸/DPI、截图、metrics、自然退出；不是产品验收） |
| `smoke-15-diagnostic` | `out/ely-native/smoke-15-diagnostic/native-run/report.json`；binary SHA-256 `6649d8d173f56213e591853719829fc9a3541a3d8452f796da31124a00e1cade`；`turn_started=1`、终态事件 `1`、network trace `5474` bytes；截图 `0013-completed.bmp`、metrics `0014-completed.json` | PASS（真实 Provider 回合链；完整产品项仍为 `pending`） |
| `smoke-15` | `out/ely-native/smoke-15/native-run/report.json`；binary SHA-256 `6649d8d173f56213e591853719829fc9a3541a3d8452f796da31124a00e1cade`；`turn_started=1`、终态事件 `1`、network trace `5466` bytes；截图 `0014-completed.bmp`、metrics `0015-completed.json` | PASS（真实 Provider 回合链；Composer 未清空且出现 `hostdraft error`，完整产品项仍为 `pending`） |
| `smoke-20` | `out/ely-native/smoke-20/native-run/report.json`；binary SHA-256 `868fc686fcd213ece93811a8787a6af0e40513f6e01c9fa3c298d2badee17967`；输入截图 `0009-typed.bmp` | failed；Enter 后无新增 `turn_started`，Journal 等待超时，network trace 与 model request records 均为 0 bytes；后续修复验证仍为 `pending` |
| `smoke-22` | `out/ely-native/smoke-22/native-run/report.json`、`process-output.json`；binary SHA-256 `17668ba0f3fc2eb25f02aae5b791f4d5e83b41fb79822d105f26cafcb3b8a19a`；`0011-typed.bmp`、`input-crop.png` | failed；click/type/key action 虽为 `passed`，但输入未命中且无新增 `turn_started`，Journal 等待超时；network trace 与 model request records 为 `0` bytes，进程自然退出、exit code `0` |
| `smoke-22b` | `out/ely-native/smoke-22b/native-run/report.json`；binary SHA-256 `17668ba0f3fc2eb25f02aae5b791f4d5e83b41fb79822d105f26cafcb3b8a19a`；`0011-typed.bmp`、`0014-started.bmp`、`0017-completed.bmp`、`metrics/0018-completed.json`、draft JSON | PASS（计划 `out/ely-release22-live-ui-plan.json`；`turn_started=1`、`terminal_event=1`、`turn_completed=1`，network trace `5466` bytes，model request records `3145` bytes，真实 prompt/assistant `native smoke ok`，draft `text` 为空且完成正文可见；仅为模型请求/Enter/Journal/清稿链通过，完整产品、像素和性能仍 `pending`，GPU 仍 `pending`） |
| `release24-calibration` | `out/ely-native/release24-calibration/native-run/report.json`、`visual-comparison/comparison-metrics.json`；binary SHA-256 `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f` | PASS（窗口、截图和 metrics 生成；与 reference6 的全图比较差异 `7.5209%`，comparison `acceptance=pending`、`assessment=not_identical`；仅为视觉证据，不是像素验收通过） |
| `smoke-24` | `out/ely-native/smoke-24/native-run/report.json`；binary SHA-256 `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f`；`0015-typed.bmp`、`0018-started.bmp`、`0021-completed.bmp`、`metrics/0022-completed.json` | PASS（计划 `out/ely-release24-live-ui-plan.json`；真实 Shift+Enter/Enter/空白 hitarea、`turn_started=1`、`terminal_event=1`、`turn_completed=1`，network trace `5466` bytes、model request records `3145` bytes，完成正文可见且 Composer 清空；未保留 isolation，不宣称 draft 精确持久化正文；完整产品、像素和性能仍 `pending`，GPU 仍 `pending`） |
| `full-24` | `out/ely-native/full-24/native-run/report.json`、`process-output.json`；binary SHA-256 `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f`；Read/Write 工具事件、`out/native-acceptance/result.txt`；`accessibility/0005-startup.json`、`0012-session-created.json` | failed；`collect_model_stream` 已返回，运行时只读诊断显示 `execute_tools` 审批路径先于对应 `tool_requested`，但 Journal 未记录具体第三 tool，无法确定实际审批；`turn_started=1`、`terminal_event=0`，等待 `turn_completed` 超时，进程 `exit_code=1`、`termination=killed-after-timeout`；失败阶段只有 `0015-permission-options.bmp`、`0018-automatic-edit-selected.bmp` 和 `0025-turn-started.bmp` 截图，没有 post-failure UIA 树 |
| `stop-reopen-24c` | `out/ely-native/stop-reopen-24c/native-run/report.json`；binary SHA-256 `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f`；`0025-before-stop.bmp`、`0030-after-stop.bmp`、`0036-session-reopened.bmp`、`0050-second-round-completed.bmp`；对应 UIA/metrics | PASS（计划 `tooling/native-gpui-tests/ely-native-stop-reopen.json`；`turn_started=2`、真实 `turn_stopped=1` 且 `reason=cancelled`、`turn_completed=1`，`terminal_event=2`，network trace `13850` bytes，进程自然退出、exit code `0`；只证明停止/重开链，未提升全部产品项） |
| `settings-dark-24` | `out/ely-native/settings-dark-24/native-run/report.json`；binary SHA-256 `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f`；八页截图/UIA | failed；真实验收缺少非空 Session Journal，Journal 与 network trace 均为 `0` bytes |
| `settings-dark-24b` | `out/ely-native/settings-dark-24b/native-run/report.json`；binary SHA-256 `56a3bc5e5be73dc0a90c7de0cc53be5659463d5434c21a07573dc4d0235fca7f`；八页截图/UIA、metrics | PASS（General、Providers、Resources、Automations、Workflows、Agents、Usage、Diagnostics 八页均有证据；`turn_started=1`、`terminal_event=1`、`turn_completed=1`，network trace `5466` bytes；仅为设置页和最小真实回合链通过，完整产品、像素和性能仍 `pending`，GPU 仍 `pending`） |
| `full-25` | `out/ely-native/full-25/native-run/report.json`、`process-output.json`；binary SHA-256 `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452`；Read/Write 工具事件、两个 marker 文件、9 份截图、6 份 UIA、6 份 metrics | PASS（计划 `tooling/native-gpui-tests/ely-native-full.json`；`turn_started=2`、`terminal_event=2`、`turn_completed=2`，3 次工具调用，Journal 事件 `25847` bytes，network trace `19638` bytes，进程自然退出、exit code `0`；只证明 release25 工具/会话重开链，完整产品、像素和性能仍 `pending`） |
| `settings-dark-25` | `out/ely-native/settings-dark-25/native-run/report.json`；binary SHA-256 `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452`；8 页截图、UIA、metrics | PASS（计划 `tooling/native-gpui-tests/ely-native-settings.json`；深色八页和最小真实回合，`turn_started=1`、`terminal_event=1`、`turn_completed=1`，network trace `5466` bytes，进程自然退出；只证明设置范围，完整产品、像素和性能仍 `pending`） |
| `settings-light-25` | `out/ely-native/settings-light-25/native-run/report.json`；binary SHA-256 `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452`；8 页截图、UIA、metrics | PASS（计划 `tooling/native-gpui-tests/ely-native-settings-light.json`；浅色八页和最小真实回合，`turn_started=1`、`terminal_event=1`、`turn_completed=1`，network trace `5466` bytes，进程自然退出；只证明设置范围，完整产品、像素和性能仍 `pending`） |
| `release25-calibration` | `out/ely-native/release25-calibration/native-run/report.json`、`visual-comparison/comparison-metrics.json`；binary SHA-256 `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452` | PASS（窗口、截图和 metrics 生成；与 reference6 比较差异 `7.4805878%`，comparison `acceptance=pending`、`assessment=not_identical`；仅为视觉证据，不是像素验收通过） |
| `permission-askuser-25` | `out/ely-native/permission-askuser-25/native-run/report.json`；binary SHA-256 `2fb479f8408370c1f727ba26b16968c001be3aa38e8c2075bcf23478c4f9a452`；startup/timeout 截图和 UIA | failed；UIA helper 默认 DPI 导致点击坐标偏移，等待“自动编辑”标签超时；属于 runner 取证失败，Journal 无新增 Turn，不能作为产品 Permission 结论 |
| `font-settings-26` | `out/ely-native/font-settings-26/native-run/report.json`；binary SHA-256 `2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94`；字体设置截图/UIA | failed；首次 runner 使用普通坐标 click，状态未稳定，最终 `assert_file` 未找到 `native-general-settings.json`；不能作为字体功能结论 |
| `font-settings-26b` | `out/ely-native/font-settings-26b/native-run/report.json`；binary SHA-256 `2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94`；7 份截图、4 份 UIA、设置文件 | PASS（通过 UIA 验证 Consolas 输入应用、离开重开和清空回退；最终 `fontFamily=系统默认`，进程自然退出、exit code `0`；仅为设置范围证据） |
| `permission-askuser-26c` | `out/ely-native/permission-askuser-26c/native-run/report.json`、`process-output.json`；binary SHA-256 `2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94`；`0021-permission-pending.bmp`、UIA 树 | failed；真实 `turn_started=1` 但无终态、network trace `0` bytes；长 JSON 撑宽聊天布局，允许/拒绝 bounds 落到窗口外，点击失败；属于当前产品布局问题，不能推出审批结果 |
| `workbench-26` | `out/ely-native/workbench-26/native-run/report.json`、`process-output.json`；binary SHA-256 `2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94`；工作台/终端截图和 UIA | failed；终端切换后等待“新建终端” UIA 标签超时，Journal 与 network trace 均为 `0` bytes；fixture Git 向父仓库发现过大 diff，native_workbench 继续排查 |
| `release26-calibration` | `out/ely-native/release26-calibration/native-run/report.json`、`visual-comparison/comparison-metrics.json`；binary SHA-256 `2de736384a637425b24df8c09fa16a12ae223fdf94808d6f7ff5de8b410ecf94` | PASS（窗口、截图和 metrics 生成；与 reference6 比较差异 `7.4805878%`，comparison `acceptance=pending`、`assessment=not_identical`；仅为视觉证据，不是像素验收通过，产品和性能仍 `pending`） |
| `permission-askuser-27` | `out/ely-native/permission-askuser-27/native-run/report.json`；binary SHA-256 `3d4412d7937f80b23ef36699594483c073b9c0cef250cc7da24c34eee3f1649c`；5 份截图、5 份 UIA、1 份 metrics、`permission-marker.txt` | PASS（两轮 Turn 均完成，`turn_started=2`、`tool_requested=2`、`turn_completed=2`，network trace `14154` bytes，Journal `19364` bytes，marker 内容精确且无换行；AskUser 严格为 A/B 多选并提交 A；仅为 Permission/AskUser 范围证据，像素和完整产品仍 `pending`） |
| `workbench-27` | `out/ely-native/workbench-27/native-run/report.json`、`process-output.json`；binary SHA-256 `3d4412d7937f80b23ef36699594483c073b9c0cef250cc7da24c34eee3f1649c`；2 份截图、2 份 UIA、1 份 metrics | failed；终端创建和输入动作完成，但等待 `terminal-marker.txt` 超时；Journal 与 network trace 均为 `0` bytes，进程自然退出、exit code `0`；终端高度根因待复测 |
| `workbench-27b` | `out/ely-native/workbench-27b/native-run/report.json`、`process-output.json`；binary SHA-256 `3d4412d7937f80b23ef36699594483c073b9c0cef250cc7da24c34eee3f1649c`；3 份截图、3 份 UIA、1 份 metrics | failed；复测已生成终端创建和命令输入证据，但等待 `terminal-marker.txt` 仍超时；Journal 与 network trace 均为 `0` bytes，进程自然退出、exit code `0`；终端高度根因待复测 |

## 构建与运行

生产 Host 使用默认 release 构建作为原生验收对象：

```powershell
cargo build -p keencode-desktop --release
```

验收器不启用桌面测试 feature 或 GPUI mock；被测程序启动后必须创建真实 GPUI/Ely
`HWND`。验收器会等待该窗口，找不到窗口或程序先退出时直接失败。

使用隔离 Provider 配置执行示例计划：

```powershell
cargo run -p keencode-native-gpui-tests -- `
  --binary target/release/keencode-desktop.exe `
  --provider-config out/native-live/native-provider-6f2.json `
  --plan tooling/native-gpui-tests/ely-native-smoke.json `
  --real-provider `
  --output out/ely-native/<run>
```

已知的隔离配置路径可以是 `out/native-live/native-provider-6f2.json` 或
`out/e2e-appdata/providers.json`。文档、终端和报告只记录配置文件路径以及
provider/model 的无凭据摘要，不记录 API key、认证 Header、私有 URL 或其它配置值。
`--real-provider` 是显式授权开关；计划声明 `requires_real_provider` 时缺少该开关会
直接失败，不以假模型或静态计划结果通过。

首次运行若要保留可复用的冷恢复根，追加 `--keep-isolation`。第二次运行使用新的
输出目录和 `--reuse-isolation <第一次输出>/native-run/isolation`：

```powershell
cargo run -p keencode-native-gpui-tests -- `
  --binary target/release/keencode-desktop.exe `
  --plan tooling/native-gpui-tests/ely-native-cold-resume.json `
  --real-provider `
  --reuse-isolation out/ely-native/<first-run>/native-run/isolation `
  --output out/ely-native/<cold-run>
```

冷恢复会严格要求来源 `report.json`、`data`、`project`、`settings.json`、
`native-general-settings.json` 和最小项目 fixture 都是既有的非符号链接路径，并确认报告
记录的 isolation 根一致。复用模式只读既有 settings、Provider 和 fixture；Provider 配置来自既有
`data/providers.json`，不会
复制命令行配置覆盖它。报告、network trace、截图、UIA 和指标写入新的 output，复用根
不会因 `--keep-isolation` 缺省而被删除。冷恢复报告同时记录当前 binary 身份与来源
报告中的上一轮 binary 身份及各自来源。

计划还可以声明 `requires_journal`、`requires_network_trace` 与
`requires_reuse_isolation`。声明 `requires_reuse_isolation` 时必须把
`--reuse-isolation` 指向既有的 `native-run/isolation`；
声明 `requires_real_provider` 或使用 `--real-provider` 会自动打开这两项强制断言：运行器必须
在隔离 data 根找到非空 `sessions/*/events.jsonl`，并在其中观察到真实
`turn_started` 及 `turn_completed`/`turn_stopped` 事件；同时必须收到被测 Host 写入的非空
`KEENCODE_NATIVE_WIRE_TRACE` JSONL。任一证据缺失都会使报告为 `failed`，截图、窗口存在、
进程正常退出和动作状态不能单独构成通过。

验收器会在输出目录下创建本次运行专用的 `native-run/isolation/data` 和
`native-run/isolation/project`，写入最小 `README.md`、`src/facts.txt`，并把传入的
`providers.json` 复制到隔离 data 根。隔离 data 根还会写入真实应用设置
`settings.json`，其中只为验收明确设置 `closeToTray=false`，以及由计划的
`native_general_settings` 写入 `native-general-settings.json`；这样验收器的真实 Ctrl+Q
关闭动作会退出进程，避免生产默认的托盘常驻把正常关闭误判为超时。两份设置只属于本次
隔离运行，不改变生产默认值，且会在报告的隔离数据清单中留下文件证据。复用模式要求两份
文件都已存在并保持只读。被测进程的当前目录也是该隔离 project，且会收到
以下约定环境变量：

| 变量 | 生产 Host 约定 |
| --- | --- |
| `KEENCODE_NATIVE_ACCEPTANCE` | 值为 `1` 时表示本次由原生验收器启动 |
| `KEENCODE_NATIVE_ACCEPTANCE_OUTPUT` | 输出证据根；Host 可写入 Journal 摘要或其它脱敏证据 |
| `KEENCODE_NATIVE_ACCEPTANCE_PROJECT` | 本次隔离项目根；Host 应把会话和工具 cwd 绑定到此路径 |
| `KEENCODE_NATIVE_WIRE_TRACE` | 可选 JSONL trace 路径；Provider 运行时按脱敏协议写入线级事件 |
| `KEENCODE_BENCHMARK` / `KEENCODE_BENCHMARK_DATA_DIR` | 仅用于显式隔离数据根选择，值分别为 `1` 和 data 路径 |

Host 应通过真实键盘和鼠标输入触发 prompt、工具、停止、权限和问题交互。验收器不
要求也不接受旁路 test mailbox、隐藏 RPC、DOM selector 或 JavaScript 注入。关闭
`--keep-isolation` 时，运行结束会删除本次新建的隔离目录，截图、UIA 和指标证据仍
保留在输出目录；只有在需要复核隔离文件时才使用 `--keep-isolation`，并应保护该
输出目录，因为其中可能包含本次授权使用的 Provider 配置副本。

## 证据与判定边界

每次运行生成 `report.json`，其中包含被测 binary 的绝对路径和流式计算的
`binary_sha256`。冷恢复报告还在 `binary_identity` 与 `reused_from.source_binary` 中
标出当前和来源 binary 的身份及来源；并按动作保存以下证据：

- `screenshots/*.bmp`：`PrintWindow` 原生窗口截图；截图存在不等于 UI 内容正确。
- `accessibility/*.json`：Windows UIA 根属性、provider 状态、通过标准
  `IUIAutomation::ElementFromHandle` 和 `ControlViewWalker` 逐层读取的有界子树，
  以及 HWND 子窗口列表。`subtree.nodes` 的节点属性来自逐节点 UIA 查询；读取上限是
  深度 4、128 个节点、单个文本属性 256 个字符。查询失败会保留根属性并写入
  `subtree.status=unavailable`，不能被解释为 UIA 验收通过。UIA 只是 AccessKit
  暴露面的操作系统读取边界；只有被测运行时实际暴露 AccessKit/UIA provider，
  才能把完整可访问树判为通过。
- `metrics/*.json`：进程 PID、Unix 毫秒 `sampledAtMs`、逻辑 CPU 数
  `logicalCpuCount`、working set、private bytes、pagefile bytes、用户和内核 CPU
  时间，以及被测窗口的 window/client rect、client 屏幕原点、DPI 和 DPI awareness。
  采样是离散证据，不是长期性能预算；这些几何字段用于复核动作计划坐标经过
  Windows DPI virtualization 后是否仍落在目标控件内。相邻样本的 CPU 百分比可按
  `((delta userCpuMs + delta kernelCpuMs) / delta sampledAtMs) * 100` 计算；若需要
  按整机逻辑 CPU 归一，再除以 `logicalCpuCount`。本仓库的 idle 计划通过 `repeat`
  采集 20 个、间隔 1500 ms 的样本，总等待窗口约 30 秒，不预设性能门槛或 PASS 结论。
- `native-run/network-trace.jsonl`：生产 Host 写入的脱敏线级 JSONL。缺失时验收器
  生成 `status: pending`；若计划要求网络证据则直接失败，不会假设网络请求成功。存在时
  只保留有限大小并按 key/token/secret/password/baseUrl 字段对应值脱敏。
- `native-run/process-output.json`：被测进程正常或非零退出后的有限 stdout/stderr、退出码、
  termination、runner 错误和读取状态。`termination` 区分自然退出与 runner 等待超时后的
  强制终止；stdout/stderr 各最多保留 128 KiB，内容以内联文本形式保存，落盘前替换已知
  敏感值并脱敏 URL、Authorization、Key、Token 等字段。
- `accessibility/*.json` 的 UIA 读取在独立 helper 进程中执行，单次最多等待 5 秒。helper
  会先流式保存 root 属性和最后阶段；若 `UiaHasServerSideProvider`、`UiaNodeFromHandle`、
  `UiaGetPropertyValue`、`IUIAutomation::ElementFromHandle` 或 TreeWalker 子节点查询阻塞，
  runner 会终止 helper，保留已有 root/child-window 证据，并将 `helper.status=timeout`、
  `helper.lastStage` 和 `subtree.status=unavailable` 写入证据，不把 provider 未响应误报为
  UIA 通过。
- `report.json.evidence.journal`：枚举隔离 data 根中的相对路径、大小和扩展名，并输出
  `events.jsonl` 文件数、总字节数、启动前 baseline 以及本轮新增的 Turn 开始/终态计数；
  不输出或复制 Journal 正文。冷恢复不能用既有 Turn 数量满足本轮断言。
- `report.json.evidence.gpu`：当前为 `pending`。本工具没有启用 Windows ETW/GPU
  Engine provider，不把 CPU 或截图结果冒充 GPU 证据。

如果被测进程在创建可见窗口前退出或超过窗口启动超时，运行目录仍会保存
`native-run/startup-failure.json`，并保存有限的 `startup-failure-stdout.txt` 与
`startup-failure-stderr.txt`。stdout 和 stderr 各最多保留 128 KiB；读取线程会持续
排空管道，进程退出后最多等待 2 秒收敛读端，超时会在清单中标记而不会阻塞验收器退出。
日志落盘前替换 Provider 配置中的已知敏感值，并脱敏 URL、Authorization、Key、Token
等敏感字段；运行器不会把这些日志正文写入终端或报告摘要。若读取线程在进程退出后
因后代进程继承管道写端而超时，运行器会取消同步读取并等待读取线程 join，不遗留持有
管道句柄的后台线程。

动作计划由真实 `focus`、`click`、`type_text`、`key`、`wait`、`screenshot`、
`accessibility`、`metrics`、`wait_for_journal`、`assert_journal_count`、`wait_for_file`、
`assert_file`、`assert_git`、`assert_window`、`repeat` 和 `close` 组成。
`wait_for_journal` 与 `assert_journal_count` 只扫描隔离 `sessions/*/events.jsonl`，
`wait_for_file`/`assert_file` 只允许隔离 project/data 根内的非符号链接普通文件；
`assert_git` 只用固定的只读 Git 查询验证隔离项目根、HEAD、clean 状态和 linked
worktree，不执行任何 Git 写操作。它们都在条件未满足或超时时使动作失败，并按进程
启动前的 Journal baseline 计算本轮新增事件，不把旧会话历史、截图、窗口存在或进程
退出当成业务成功。
`ely-native-full.json` 是包含真实建会话、工具生命周期、文件标记、重新打开后第二轮
发送以及第二个会话创建的 Provider 验收计划；`ely-native-stop-reopen.json` 覆盖持续
工具执行期间的真实停止和再次打开；`ely-native-smoke.json` 提供不执行工具的最小流式
回合。

`ely-native-workbench.json` 在 PTY 命令和编辑器保存后读取标记文件，提交后要求 Git
HEAD 标题匹配且工作区 clean，创建工作树后要求 Git worktree 数量和新分支均匹配。
`ely-native-cold-resume.json` 声明 `requires_reuse_isolation`，没有复用既有
`native-run/isolation` 会直接失败，并在发送第二轮前只读检查两份 settings、Provider 和
项目 fixture，避免在新建空根上把冷恢复误报为通过。

正式的 `full`、`stop-reopen` 和 `cold-resume` 计划都声明固定的深色
`native_general_settings` 与 `visual` 环境，并通过 `assert_window` 校验 client
`2560x1640` 和 DPI `192`。`full` 的首轮 `tool_requested` 必须匹配 Read 的工具名、
`src/facts.txt` 参数和 `read_only` effect；各计划的第二轮使用独立输出路径和 marker，
Journal 查询与文件内容都按该独立标记校验，不能由上一轮累计的工具计数满足。上述计划
只定义验收边界，不代表已经完成坐标或产品功能验收。若 `accessibility` 证据的 UIA
provider 或 subtree 读取失败，相关产品项继续保持明确的 `pending`，不能把动作完成或
截图存在解释为 UIA 通过。

仓库还提供以下面向关键原生交互的计划。它们使用同一套真实输入、Journal、截图和
UIA 证据链；坐标必须先用同一 DPI 环境的截图、metrics 和 `subtree.nodes` 校准，再把
运行结果作为验收证据。

| 计划 | 覆盖范围 | 关键事实证据 | 当前状态 |
| --- | --- | --- | --- |
| `ely-native-permission-askuser.json` | 真实 Provider 回合、工具权限审批、`ask_user` 原生问题与回答 | `tool_requested`、`tool_execution_started`、`tool_completed`、项目标记文件、两组 pending/resolved UIA 与截图 | 待校准、待真实 Provider 运行 |
| `ely-native-goal-workflow.json` | Settings 中 Goal 创建、Workflow 定义保存、单层 actor 执行和完成 | Goal/Workflow 页面 UIA、`workflow_event_committed`、Turn 开始/完成 Journal | 待校准、待真实 Provider 运行 |
| `ely-native-settings.json` / `ely-native-settings-light.json` | 深色/浅色 General、Providers、Resources、Automations、Workflows、Usage、Diagnostics 页面 | 隔离 settings/project fixture 只读断言、各页面截图、UIA 子树和页面指标 | 待校准、待原生窗口运行 |
| `ely-native-workbench.json` | PTY Terminal、项目文件编辑、Git 提交和 Worktree 创建 | PTY/编辑器文件只读断言、Git HEAD clean/提交标题/worktree 数量与分支、页面 UIA 与截图 | 待校准、待原生窗口运行 |

当前 `sourceBaseline` 目录保存的是固定来源提交的深色参考图；浅色计划复用来源提交、尺寸和
DPI 元数据，但在生成同主题浅色参考图前，不把它解释为浅色像素比较通过。

`click` 的 `x`、`y` 是被测 HWND client 坐标，runner 直接交给
`ClientToScreen`，再用 `SetCursorPos` 和 `SendInput` 注入左键。runner 不把截图像素
或窗口左上角当成独立坐标系；每个 metrics 证据会同时记录 client 原点、窗口 DPI 和
双方的 DPI awareness。runner 在启动被测进程前必须成功设置 PerMonitorV2，因此这些
坐标和 `PrintWindow` 截图统一使用物理像素；设置失败会直接终止本次验收。计划校准
视觉计划中的 `visual` 字段固定逻辑窗口 `1280x820`、DPI `192`、DPR `2` 和物理 client
`2560x1640`，并引用 `out/native-live/zcode-source-baseline-29628c9-dpr2`；计划的
`assert_window` 会检查实际 client 尺寸和 DPI。runner 不主动改变桌面或窗口尺寸，生产
Host 已按同一逻辑尺寸创建窗口；若 metrics 不满足该断言，必须重新校准坐标和截图，不能
继续使用旧坐标。当前正式计划中的 Composer 点击坐标 `x=1000,y=1320` 和 Stop 点击坐标
`x=1200,y=690` 仍等待根基于新的截图、metrics 和 UIA 子树证据重新校准；在校准完成前，
这些坐标只能作为待复核计划输入，不能据此宣称交互或产品验收 `PASS`。

`focus` 先为 runner 建立消息队列，尝试将当前线程与前台/目标线程合并，并保留
 `SetForegroundWindow` 的严格结果检查。若 Windows 前台策略拒绝该调用，runner 只发送一次
真实 Alt 输入并重试；仍失败时才用当前 DPI 下 client 顶栏中点（逻辑 `y=12`）作为最后一次
有界 fallback。fallback 先用 `WindowFromPoint` 与 `GetAncestor(GA_ROOT)` 确认命中根节点就是
目标 HWND，再通过真实 `SendInput` 左键点击，最后仍必须满足
`GetForegroundWindow() == target HWND`；校验失败会使动作失败。runner 不用 `PostMessage`、
不绕过前台断言，也不向其它窗口发送关闭或 Escape 输入。

定位能力审计：当前所有计划的 `click` 仍使用上述 HWND client 坐标；runner 的
`accessibility` 动作只采集 UIA 根节点和标准 `IUIAutomation` TreeWalker 子树证据，没有 UIA locator 或
基于 UIA 的 click 动作，因此不能把这些计划描述为已全面使用 UIA 定位。可复用的
键盘辅助仅限计划中已有的标准按键：`Enter` 用于提交或确认，`Ctrl+A` 用于编辑器
全选，`PageDown` 用于 Workflow 页面滚动；其它页面切换、按钮和表单聚焦仍依赖坐标。
未有校准证据的坐标仍保持待校准状态，不应据此宣称对应控件交互已通过。

## 生产 Host 接口要求

生产二进制启动时应创建真实 HWND，并让 GPUI/Ely 的 AccessKit/UIA surface 随窗口
生命周期建立和释放。Host 可以在读取 `KEENCODE_NATIVE_ACCEPTANCE=1` 后，将
`KEENCODE_NATIVE_ACCEPTANCE_OUTPUT`、`KEENCODE_NATIVE_ACCEPTANCE_PROJECT` 和
`KEENCODE_NATIVE_WIRE_TRACE` 作为显式的测试证据目的地；普通启动不得因为这些变量
缺失而创建伪窗口或旁路运行时。

Journal 仍由 Rust Host 持有权威事实。验收器只检查隔离目录清单，避免把报告工具
变成第二个 Journal。网络 trace 若由 Host 写入，必须在写入前移除认证 Header、令牌、
API key 和包含凭据的 URL；验收器的二次脱敏只是边界保护，不能替代 Host 的日志策略。

## 范围排除

本工具和本矩阵不验收 doc、pdf、office、media、html、browser、web 或 mobile 功能。
`PrintWindow` 生成的 BMP 仅是桌面窗口证据格式，不代表 media 产品范围；UIA 读取也
不代表 browser/web 自动化。上述范围如需验收，应使用各自明确的原生或文件格式验收
工具，不能将 Ely/GPUI 窗口 smoke 结果扩展解释为通过。

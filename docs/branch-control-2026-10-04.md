# 新建对话分支入口重复修复

## 原因与修改

原有 `GitBranchSwitcher` 与 KeenCode 的 `ComposerWorktreeMenu` 并排显示。
后者也使用分支图标和当前分支名，导致新建对话出现两个相同的 main 按钮。

保留原分支选择器；工作树入口使用独立文件夹/Git 图标和本地化“工作树”名称。
菜单继续保留创建、切换、会话交接、归档和移除。当前工作树身份仍来自 Rust 查询，
tooltip 展示目录与分支，验收通过 `data-worktree-path/branch` 核对真实投影。
没有修改 Rust 业务协议、Git 仓库、用户模型配置或 ZCode 源仓库。

## 验证与证据

| 检查 | 结果 |
| --- | --- |
| 原生 Tauri/WebView2 | `out/native-live/branch-duplicate-worktree-regression-final/report.json`：96/96 PASS，frontendFaults=0、protocolFaults=0；包括真实 deepseek-v4.1-flash 会话和工作树生命周期 |
| 新建对话控件 | 同目录 `draft-header-controls.json`：branchSelectors=1、branchLabels=[master]、worktreeLabel=工作树、独立图标；Git fixture 用 master，修复不依赖分支名称 |
| 原生截图 | 同目录 `draft-single-branch-and-worktree.png`，已人工复核实际布局 |
| build/typecheck | `out/branch-duplicate-build-final.log` PASS，包含强制 typecheck |
| 前端测试和门禁 | `out/branch-duplicate-tests.log`：28 files / 90 tests PASS，scripts 74 PASS / 1 macOS skip，来源/设计门禁 PASS |
| CSS | `out/branch-duplicate-css.log` PASS |
| 原生构建 | `out/branch-duplicate-desktop-build.log` PASS；保留 Windows linker 输出 warning |
| 当前原生计划语法 | `out/branch-duplicate-plan-syntax.log`：28 plans / 850 expressions PASS；未执行的计划不因此计为功能通过 |
| 工作区与来源 | `git diff --check` PASS，ZCode 源仓库仍干净，无提交或推送 |
| 凭据扫描 | `out/native-live/credential-scan-branch-duplicate.json`：源码/最终产物的 apiKey、baseUrl blocking 命中均为 0 |

执行命令：`node tooling/scripts/native-live-e2e.mjs --plan tooling/native-live/native-worktree-gui-plan.json --provider-config out/native-live/native-provider-6f2.json --binary target/debug/keencode-desktop.exe --output out/native-live/branch-duplicate-worktree-regression-final --port 9518`。

最终 EXE 110076416 bytes，SHA256
`214a16fd22bbf05b4936a1e0a6fdf4d6cabc536026cd0b2ec4c92d3f10edaa55`。
来源/产物摘要见 `out/native-live/frontend-provenance-branch-duplicate.json`，dist tree SHA256
`7f2604f837cf82be132807e674a551ab7b137fbf7684d9b8325afa212463a10c`。
原生回归耗时 30133ms，首次可交互 1244ms，不作为完整资源性能基准。

首次回归在 UI 断言处停止：fixture 在 Composer 挂载后才初始化 Git，工作树列表在菜单打开时刷新，
尚未加载的 worktreeBranch 不能作为布局前置断言。随后将布局断言限定为真实控件数量/名称/图标，
身份与生命周期继续在真实列表加载后核对；前置失败报告保留，最终回归通过。

改前源码、计划和旧 EXE 备份于 `out/native-live/branch-duplicate-20261004-preimage/`。
已沿用原 `keencode-manual-88417ca6` 数据根和 WebView profile 重启桌面，启动记录见
`out/native-live/manual-launch-branch-20261004.json`；真实原生目录选择器未被测试覆盖替代。

本次只对上述范围重新验收，不扩展到此前矩阵中的全部功能或 OS 手工待验证项。

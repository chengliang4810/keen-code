# 左下角设置入口调整

账户头像和 KeenCode 用户名占位已移除，合并为“齿轮＋设置＋下拉箭头”。
菜单首项打开完整设置，分组保留主题、语言和桌面缩放；工作区不再显示重复齿轮。
设置页保留共享菜单与独立返回箭头，打开设置项禁用，避免误调用返回回调。
使用原有 UI 控件、主题令牌、国际化键和 Rust 设置/窗口服务，没有增加依赖。

| 验证 | 结果与证据 |
| --- | --- |
| 类型 / 构建 | `out/sidebar-settings-menu-build.log`：typecheck、Vite PASS |
| 前端 / 脚本测试 | `out/sidebar-settings-menu-tests.log`：28 files / 90 tests PASS；脚本 74 PASS、1 macOS skip |
| CSS / 设计 / 来源 | `out/sidebar-settings-menu-css.log` PASS；pnpm test 中设计、clean-room 门禁 PASS；ZCode 固定提交工作树干净 |
| 原生构建 | `out/sidebar-settings-menu-desktop-build.log` PASS；仅保留既有 Windows linker 输出 warning |
| Windows 原生 WebView2 | `out/native-live/sidebar-settings-menu-native-stable/report.json`：62/62 PASS，frontendFaults=0、protocolFaults=0 |
| 真实交互 | 单一入口、菜单分组、Escape、浅色/深色、中文/英文、工作区放大、设置页恢复缩放、返回及 Composer 草稿保留均 PASS |
| 冷启动 | 同一隔离配置冷启动后保留中文/深色偏好，菜单 radio 选中项一致 |
| 截图 | 上述目录的 `workspace-settings-menu.png`、`settings-page-preferences-menu.png`、`workspace-settings-entry-dark.png`，已复核工作区入口和设置页返回按钮 |
| 来源与产物 | `out/native-live/frontend-provenance-sidebar-settings-menu.json`：1147 一致、165 适配、238 裁剪、9 新增 |
| 桌面恢复 | `out/native-live/manual-launch-sidebar-settings-menu-20261004.json`：同一数据目录/WebView profile，PID 39600，KeenCode 窗口响应正常，目录选择器测试覆盖关闭 |

原生验收命令：
`node tooling/scripts/native-live-e2e.mjs --plan tooling/native-live/sidebar-settings-menu-plan.json --provider-config out/native-live/native-provider-6f2.json --binary target/debug/keencode-desktop.exe --output out/native-live/sidebar-settings-menu-native-stable --port 9518`。
本次仅验证设置菜单，隔离模型请求记录为 0 bytes，不计为真实模型测试。

本次 UI 差异仅在 `WorkspaceSidebarFooter.tsx` 和 `SettingsPage.tsx`。
另外同步 17 个既有原生计划的 35 处设置入口动作：入口展开菜单后通过真实鼠标
点击“打开设置”，返回动作不额外点击菜单项。这些历史计划没有全部重新运行。
首次原生计划在设置页断言中选中了后台保留的工作区 footer；限定到 settings-page
后重跑 60 步全部通过，并追加两项等待菜单淡入完成的截图检查，最终 62 步通过。保留首轮报告，不把断言失败归因于产品故障。

EXE 110147584 bytes，SHA256
`4102b31860fdeff1a28623dba1ea0e2c664cd8cb98eef32683d9aa4f716f7e9c`；
dist tree SHA256
`4e52352030bb5f4ae4b7f8be61ab5af66f8b571c87477a5904f5f2e554675c08`。
来源、旧源码、既有计划和旧 EXE 备份见 `out/native-live/sidebar-settings-menu-preimage/`。

未修改 Rust 业务逻辑，没有重跑 Rust workspace 单测或完整历史功能验收。
macOS/Linux、非桌面入口没有实机验证；本次没有单独资源采样，不作 CPU/内存优化结论。
`git diff --check` 通过，没有提交或推送 Git。

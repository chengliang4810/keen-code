# 侧边栏切换图标调整

左上角入口始终显示侧边栏图标，移除默认 Logo 与 hover 图标的交替显示。
展开和收起状态继续使用对应的 `PanelLeftClose` / `PanelLeftOpen`，保留点击、快捷键、
tooltip 和按钮尺寸。折叠侧栏入口采用同一规则，并清理五个来源文件中的 Logo 参数。
固定 ZCode 基线、许可证与适配边界记录在 `third-party/zcode/SOURCE-MAPPING.md`。

| 检查 | 结果与证据 |
| --- | --- |
| Windows 原生 WebView2 | `out/native-live/sidebar-icon-native-regression-final/report.json`：21/21 PASS，frontendFaults=0、protocolFaults=0 |
| 默认/hover 图标 | 同目录 expanded-idle/expanded-hover JSON SHA 相同；collapsed-idle/collapsed-hover JSON SHA 相同。四种状态均无 img，SVG opacity=1；真实鼠标收起/展开通过 |
| 截图 | 同目录 `expanded-sidebar-icon.png`、`collapsed-sidebar-icon.png`，已复核左上角图标 |
| 前端 | `out/sidebar-icon-build.log`：typecheck + Vite PASS；`out/sidebar-icon-tests.log`：28 files / 90 tests、scripts 74 PASS / 1 macOS skip，设计/clean-room 门禁 PASS |
| CSS / Git | `out/sidebar-icon-css.log` PASS；`git diff --check` PASS；ZCode 固定提交工作树保持干净 |
| 原生构建 | `out/sidebar-icon-desktop-build.log` PASS，保留既有 Windows linker 输出 warning |
| 来源与产物绑定 | `out/native-live/frontend-provenance-sidebar-icon.json`：对实际 `D:/projects/ZCode/packages/ui/src` 比对，1147 一致、165 适配、238 裁剪、9 新增 |
| 桌面重启 | `out/native-live/manual-launch-sidebar-icon-20261004.json`：保留原数据根和 WebView profile，PID 37072、窗口 KeenCode 响应正常，无原生目录选择器测试覆盖 |

原生执行命令：
`node tooling/scripts/native-live-e2e.mjs --plan tooling/native-live/sidebar-icon-plan.json --provider-config out/native-live/native-provider-6f2.json --binary target/debug/keencode-desktop.exe --output out/native-live/sidebar-icon-native-regression-final --port 9518`。
本次为界面动作验收，没有发起模型对话，不将报告中的模型配置当作真实模型测试结果。

EXE 为 110147584 bytes，SHA256
`6eef2dd26179f33cf4ba27aa36d3414c583012f603836125fcce2080f366d7ff`；
dist tree SHA256 为 `8d441e13a2e13e31c779583860cfab2c1f21543f04258d045d4b0061d92862ed`。
改前源码和 EXE 备份见 `out/native-live/sidebar-icon-20261004-preimage/`。

首次原生计划在侧栏收起动画期间试图 hover 正在移动的 Composer，验收器拒绝不稳定坐标。
随后将移开鼠标的目标改为固定标题栏按钮，最终 21 步全部通过；首次报告保留。
前端构建保留既有 chunk/dynamic-import 提示，没有为此次界面调整修改 Rust 业务逻辑，
也没有重新运行 Rust 单测或此前完整功能验收。macOS/Linux 与非桌面折叠轨道未做原生实机验收。
没有提交或推送 Git；可直接在已启动的桌面使用。

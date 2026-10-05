# 手工反馈修复验收（2026-10-04）

## 结果与范围

三项反馈已修复，独立 Windows Tauri/WebView2 回归 **53/53** 通过。
这一轮是 Native49 之后的新构建；旧 25 scope / 2007 steps 报告仍是历史证据，
不用于宣称新构建已重新完成全部功能验收。无 Git 提交或推送。

| 反馈 | 修改与验证 | 状态 |
| --- | --- | --- |
| 引导不收集工作方向，不选择 UI 模式 | 直接进入记忆偏好页，一次跳过即可完成；常规设置、侧栏菜单移除模式选择；store 对读取、设置和跨窗口广播统一固定 coding。隔离 profile 写入旧 office 偏好后冷启动，模式选择不恢复 | PASS |
| 模型设置不显示两个供应商小标题 | 仅显示自定义供应商列表，无两个分组标题；真实 provider 详情、Chat Completions 控件及 deepseek-v4.1-flash 行仍加载 | PASS |
| 插件/命令页内部对话目录登记错误 | 不添加项目，默认 conversation 上的真实资源 RPC 完成加载，无“项目尚未添加”；用户命令保存、回读、冷恢复、禁用、启用、确认删除全部通过 | PASS |

命令回读还发现并修复了目录协议不一致：命令的 `scope=global` 保留不变，
但 `location.scope=user` 才能进入源前端用户目录筛选；写入和更新返回 `{ command }`。
资源读取只允许精确的 conversation 根，子目录、兄弟目录和其他未登记路径仍拒绝。
默认对话不因此取得项目写入权限，用户命令删除/启停仍限定用户配置根。
保留原引导完成设置字段作为兼容标记，本地记录 `occupation=null`、`interfaceMode=coding`，
不采集工作方向。来源映射与协议说明已更新。

## 可复核证据

- 原生命令：`node tooling/scripts/native-live-e2e.mjs --plan tooling/native-live/manual-feedback-plan.json --provider-config out/native-live/native-provider-6f2.json --binary target/debug/keencode-desktop.exe --output out/native-live/manual-feedback-native-regression-final --port 9518`。
- 报告：`out/native-live/manual-feedback-native-regression-final/report.json`。53/53，frontendFaults=0、protocolFaults=0，原始 frontend-errors 为空。
- 截图：同目录 `onboarding-preferences.png`、`general-coding-only.png`、`conversation-plugin-loaded.png`、`conversation-commands-loaded.png`、`conversation-command-cold-restored.png`。模型详情只作 DOM 断言，不截图敏感配置。
- Windows `10.0.26100`、Node `24.13.0`、pnpm `10.14.0`；总耗时 7797ms，首次可交互 1403ms。这不是完整 CPU/内存性能基准。
- 最终 EXE：110080512 bytes，SHA256 `e27b7fae2bb1e6668439ca47901ec530a2504987769fd297a16fde3087f054a2`。
- 来源/产物：`out/native-live/frontend-provenance-manual-feedback.json`；dist 3640 files，tree SHA256 `170b16f9ee4f1f141328e357f17757e69f8eea7272910de1b19d4f8b28910da8`。

## 离线检查

| 检查 | 结果与日志 |
| --- | --- |
| typecheck、build | PASS；`out/manual-feedback-typecheck.log`、`out/manual-feedback-build-second.log`（build 包含强制 typecheck） |
| pnpm test（含来源/设计门禁） | 28 files / 90 Vitest PASS；scripts 74 PASS / 1 macOS skip；`out/manual-feedback-frontend-tests-final.log` |
| CSS | PASS；`out/manual-feedback-css-final.log` |
| cargo fmt --all -- --check | PASS |
| cargo test -p keencode-desktop --all-targets | 1138 PASS / 5 ignored；`out/manual-feedback-rust-tests-final.log` |
| cargo clippy -p keencode-desktop --all-targets -- -D warnings | PASS；`out/manual-feedback-clippy-final.log` |
| cargo build -p keencode-desktop --features native-desktop-tests | PASS；`out/manual-feedback-desktop-build-final.log`（Windows linker 输出库文件的 warning 保留） |
| git diff --check、ZCode 源仓库状态 | PASS；源仓库仍无修改 |
| 凭据扫描 | PASS；`out/native-live/credential-scan-manual-feedback.json`，源码与最终产物的 apiKey/baseUrl blocking 命中均为 0 |

28 个当前验收计划同步为单步引导；历史 debug/retry/frozen 计划、ZCode 参考计划和
既有执行快照保留。这里只执行本轮 53 步计划，不把其余更新后的计划记为重新通过。
备份：`out/native-live/manual-feedback-20261004-preimage/`，包括源码清单、计划原文及旧 EXE。
两次前置原生失败分别暴露引导说明残留文案、已保存命令被目录 scope 隐藏，记录保留在
`manual-feedback-native-regression`、`manual-feedback-native-regression-second`。
首次门禁发现旧验收文档中的退役前端名称残留，已改为引用 `docs/source-history.md`，
未放宽门禁；原失败日志保留。

## 桌面启动与验证边界

最终桌面已重新启动，PID 18612，窗口 KeenCode、有窗口句柄且 Responding=true。
沿用原手工测试 `keencode-manual-88417ca6/data` 和同一 WebView profile，保留配置、会话、项目；
未启用原生目录选择器覆盖。启动记录为 `out/native-live/manual-launch-feedback-20261004.json`。

本轮使用已授权真实 provider 配置加载设置，测试真实原生 UI/Rust RPC 和持久化；
未新增模型请求，不把设置控件加载当作真实推理。此前模型全链路验收仍归 Native49。
OS picker、Explorer 拖入、系统通知和托盘的手工待验证范围仍见上一轮矩阵。

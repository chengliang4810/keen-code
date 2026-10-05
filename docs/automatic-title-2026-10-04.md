# 会话自动标题修复与验收

## 完成结果

用户指定会话 `session-7c67f2baa9b73c78eca269b3eae71415d740ced9a922d9d7522a84c0ae43c7be`
已通过原生界面重新打开，由原配置的 `deepseek-v4.1-flash`（OpenAI Chat Completions）
真实生成标题“当前项目用途说明”。侧栏标题与 Journal 一致，来源为 `automatic`。
模型请求台账按 request ID 合并 running/success 生命周期记录后只有一次标题请求，HTTP 200。

原有 47 条 Journal 记录的完整字节前缀保持不变，只追加 `title_generated` 和
`session_renamed` 两条事实，sequence 为 49；未添加用户消息、重放工具或直接修改数据文件。
再次沿用原数据根和 WebView profile 冷启动后，标题与 Journal SHA256 均保持不变。
原生桌面已启动，真实目录选择器可用，临时本机调试端口已关闭。启动记录见
`out/native-live/manual-launch-title-20261004.json`。

## 原因与关键决策

旧 ACP 路径使用了 Rust 的隔离标题生成接口，但新 V4 `sendText` 没有安排自动命名，
且该生成接口只缓存结果，不提交会话改名，因此默认“新对话”一直保留。

- 首次发送成功、打开旧根 conversation 共享 Rust 后台入口。先订阅再读快照，
  如首条输入未持久化则等待 Journal；切换界面不会取消这个输入确认等待。
- 主题取首条真实根用户文本，排除 meta 和子 Agent 所属 Turn，最多 4000 字符。
  消息 ID 的 SHA256 形成稳定 operationId。默认未命名或 message-prefix 可生成，
  手工标题、已生成标题、自定义创建名称保持原样。
- 复用现有 Provider、60 秒超时、取消和持久单飞缓存，不携带业务工具，不污染聊天正文。
  缓存与改名使用不同控制操作 ID，避免两个事实共用 Journal 去重键。
- Runtime 在同一控制锁中比较原标题及来源后写入自动改名，防止网络等待期间的
  手工改名被覆盖。重复调用、迟到结果、冷恢复都不重复提交。

## 验证与证据

| 检查 | 实际结果与证据 |
| --- | --- |
| 原生真实模型回归 | `out/native-live/title-feedback-native-regression/report.json`：50/50 PASS，frontendFaults=0、protocolFaults=0；新对话真实回复、自动命名、侧栏/标题栏同步、两次重启、手工标题和连续对话 |
| 原生 Journal 去重 | 同报告：title_generated=1、自动 session_renamed=1；手工改名后总 session_renamed=2，两次冷启动摘要不变 |
| 指定旧会话修复 | `out/native-live/title-feedback-user-session/ui-title-evidence.json`、`journal-repair-evidence.json`：标题/来源、真实请求 HTTP 200、原 Journal 前缀和冷恢复摘要一致 |
| 截图 | `title-feedback-native-regression/automatic-title.png`、`manual-title-cold-recovery.png`；`title-feedback-user-session/target-title-row.png` 仅截取指定标题行，已复核 |
| desktop 单测 | `out/title-feedback-desktop-tests-final.log`：1141 PASS、0 failed、5 ignored；包含首轮命名、8 次重复调度、隔离 HTTP 请求与正文不变检查 |
| runtime 单测/集成 | `out/title-feedback-runtime-tests.log`：4 suites / 169 PASS；包含原子自动改名、竞争结果、手工改名保护、冷恢复和幂等冲突 |
| 严格 Clippy | `out/title-feedback-clippy-final.log`：`cargo clippy -p keencode-runtime -p keencode-desktop --all-targets -- -D warnings` PASS |
| 格式/设计/来源 | `cargo fmt --all -- --check`、`git diff --check`、设计和 clean-room 门禁 PASS；ZCode 来源仓库保持干净 |
| 原生构建 | `out/title-feedback-desktop-build.log` PASS，保留 Windows linker 输出 warning |
| 来源绑定 | `out/native-live/frontend-provenance-title-feedback.json`：当前源码、未改动的前端 dist 与新 EXE 摘要 |
| 凭据扫描 | `out/native-live/credential-scan-title-feedback.json`：修改/未跟踪源码、当前报告/截图、来源摘要与 EXE 的 blockingHits=0；私有配置和用户数据备份不属于公开产物扫描范围 |

原生执行命令：
`node tooling/scripts/native-live-e2e.mjs --plan tooling/native-live/automatic-title-plan.json --provider-config out/native-live/native-provider-6f2.json --binary target/debug/keencode-desktop.exe --output out/native-live/title-feedback-native-regression --port 9518`。

EXE 为 110148096 bytes，SHA256
`58120fa509e8a21f952c3862ddc0e4f4e2727d07e07680e18369252b5b9779f7`。
原生回归 16518ms，首次可交互 1271ms；单次 debug 进程树空闲采样 CPU 0.2435%，
working set 812314624 bytes、private 734134272 bytes，不作为 release 性能预算。

改前源码、EXE 和指定会话备份见 `out/native-live/title-feedback-20261004-preimage/`。
首次单测发现缓存和改名操作 ID 冲突，修复后全量相关单测通过；旧会话 UI 验收首次
点击处于展开动画中的行未进入会话，改为等待稳定布局和鼠标命中后通过。失败日志保留，
只以最终报告计数。本次没有提交或推送 Git。

本次未重跑前端类型/构建/90 tests（前端业务源码未改变），没有重新验收此前完整
Native49 full25，也没有重跑整个 Rust workspace。其他 OS 手工待验证项继续保持 pending。
建议直接在已启动的桌面继续使用；其他旧默认标题会话在打开时补生成。

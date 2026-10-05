# 原生端到端性能排查与修复（2026-10-05）

## 本次结论

真实 Windows Tauri/WebView2 性能计划 58/58、功能与停止后 GC 补验 93/93 通过。
使用隔离配置中的 OpenAI Chat Completions / `deepseek-v4.1-flash`，模型回复、Read、
Write、Bash、连续对话、停止和冷恢复均经过真实前端、Rust 网关、Runtime 和 Journal。
不以模拟响应或浏览器开发服务器代替桌面验收。

本次将工作区全量索引、匹配和排序迁到 Rust，删除前端搜索 Worker；按实际预览生命周期
管理高亮 Worker，并修复流式代码重复重建 DOM 和无界高亮缓存。原配置桌面已恢复，
30 秒空闲采样为 609.79 MiB RSS、366.96 MiB 私有提交、整机 CPU 0.0258%。
桌面及 WebView2 进程树约 600 MiB 的基础占用仍存在，不能将本次结果表述为内存问题全部解决。

## 发现与处理

| 事实与证据 | 修复 | 验证边界 |
| --- | --- | --- |
| 旧 Rust 全量文件接口返回的打包格式与 TS 解码契约不同，20002 文件夹具的搜索返回零匹配；原方案重复扫描、传输、解码并在 Worker 复制索引 | 新增 `frontend_rpc/workspace_search.rs`：Rust 保存唯一索引、执行匹配和有界 top-K；界面只接收最多 1000 个原 `WorkspaceFileEntry` | 20002 文件中搜索 400 个匹配；文件树和命令中心均通过原生界面验证 |
| 原 Root 在空闲页面也启动四个高亮 Worker | Provider 移至实际 File/Diff，最后一个卸载释放共享池；最多两个 Worker，AST 缓存由 100 改为 16 项 | 空闲 Worker 数 4→0；完成代码回复后需要高亮时为 2；关闭预览后的功能补验为 0 |
| 代码内容变化被用作 React `key`，流式 token 反复重新创建 File、Shadow DOM、CSS 和事件监听 | 保留 `file.cacheKey` 供 AST 缓存使用，移除组件内容 key，复用上游支持更新的 File 实例 | 实际 CPU profile 定位 `applyFullRender` / CSS 注入；1500 行预览及内容更新通过 |
| 轻量 Shiki token 缓存无容量限制，首尾摘要键可能把中段不同的代码当成同一结果，多个订阅会重复 tokenize | 完整内容键；64 项 / 估算 2 MiB token 预算；同键在途合并；4 个高亮器 LRU，并等待在途使用结束再 dispose | 三个专项单测覆盖碰撞、合并、淘汰和失败重试；本次未单独测出它对整机 RSS 的降幅 |

Rust 索引最多缓存两个根，每根为估算内容 32 MiB / 250000 项。这个预算不是进程 RSS
硬上限，容器容量、扫描临时分配及在途请求仍有开销。超预算显式提示 `.zcodeignore`，
不静默裁掉目录尾部。复用现有 `ignore` 和原生 Notify 依赖，没有新增运行时语言宿主。
原生事件使缓存失效，扫描途中发生变动不会丢掉通知；连接关闭清理 owner 和 watcher，
关闭后迟到的 blocking 请求不能重新获得租约。跳过符号链接和 Windows reparse point。

保留来源 ZCode 的 DOM、主题、键盘语义和 WASM 高亮。将目录计算放到 Rust 可减少
跨 IPC 和 Worker 复制；浏览器的布局与绘制仍由 WebView2 承担，换语言不能消除这些开销。

## 测量口径与数据

Windows / WebView2 148、原生 debug 构建、1280×820、DPR 2、20 个逻辑处理器。
只统计本次桌面及其后代进程；CPU 用进程树累计 CPU 时间除以采样时长和逻辑处理器数。
RSS 与私有提交取每个时间窗末尾，**不是峰值采样**。V8 数据分别取主页面和 Worker，
主页面 GC 后的堆不能代表 Rust、GPU 或整个 WebView2 私有提交。

| 场景 | 构建/运行 | RSS MiB | 私有提交 MiB | 整机 CPU |
| --- | --- | ---: | ---: | ---: |
| 隔离新会话空闲 | 旧构建 `performance-before-v3` | 718.34 | 589.63 | 0.0708% |
| 隔离新会话空闲 | 最终性能 `performance-final` | 667.19 | 434.29 | 0.0356% |
| 工作区搜索后 | 最终性能 | 682.86 | 429.72 | 0.1761% |
| 1500 行代码预览 | 最终性能 | 793.69 | 621.96 | 0.0995% |
| 真实代码流式回复，20 秒窗末尾 | 最终性能 | 941.45 | 731.51 | 4.9004% |
| 回复结束后 | 最终性能 | 898.58 | 734.87 | 0.0994% |
| 后续六个 30 秒空闲窗 | 最终性能 | 875.98–878.29 | 708.48–709.98 | 0.0202%–0.0580% |
| 停止后空闲，30 秒 | 最终功能 `performance-functional-final` | 738.00 | 512.21 | 0.1339% |
| 原配置恢复后的日常桌面，30 秒 | PID 28588 | 609.79 | 366.96 | 0.0258% |

最终性能计划主页面 GC 后堆为 39269716→39271080 字节（约 37.45 MiB），
后续六窗约三分钟内仅增加 1364 字节；DOM 保持 8678 节点、635 个监听。
另一次真实写文件、命令、冷恢复和停止补验中，即时堆约 145.40 MiB、12563 个监听，
后续 GC 后为 34631660→34604416 字节（约 33 MiB）、772 节点、609 个监听。
未看到这两段短曲线中的持续保留增长，不能据此排除小时级泄漏。

第一次优化后的 `performance-after` 仍出现 1695.91 MiB 的流式窗末尾样本，因此继续
采集 CPU profile 并处理内容 key。该次主页面 profile 为 32.96 秒，File 全量渲染 self
采样约 2.92 秒；移除 key 后最终 profile 为 66.14 秒，全量渲染约 1.03 秒。
最终时间线同步布局仍占约 9.03 秒，是后续布局调度优化的候选。

真实模型输出不固定：旧 v2 / 旧 v3 / 第一次优化 / 最终性能的 reasoning 字符数分别为
35090 / 182388 / 3718 / 36140。旧构建流式期间两次主页面 CDP 超时，Journal 中 Read
和 turn 已确认完成，但不能将超时完全归因于渲染，也不能把 1696→941 MiB 表述为
受控峰值降低 44.5%。上述 profile 和内存样本仅用于定位与观察；严格降幅需要冻结同一
历史和输出的 A/B 对照。

## 真实功能与离线验证

| 检查 | 结果与证据 |
| --- | --- |
| 原生性能，20002 文件、预览、真实 Read 和双代码围栏回复、CPU profile、两次堆、六次空闲采样 | 58/58；`out/native-live/performance-final/report.json`；27 次真实宿主 IPC 健康检查；frontend/protocol fault=0 |
| 原生文件索引失效、忽略规则、预览外部更新、命令中心、真实 Read/Write/Bash、物理文件、冷恢复、连续对话、流式后停止、零活跃会话及两次 GC | 93/93；`out/native-live/performance-functional-final/report.json`；15 次宿主 IPC 健康检查；frontend/protocol fault=0 |
| UI 全部 Vitest | 29 files / 93 PASS；`out/performance-delivery-ui-tests.log` |
| 原生验收脚本单测 | 76 PASS / 1 macOS skip；`out/performance-script-tests.log` |
| desktop 全部单测 | 1152 PASS / 5 ignored；`out/performance-rust-tests.log` |
| 后续加强的真实 Notify 专项单测 | 2 PASS；`out/performance-search-notify-tests.log`，覆盖事件失效和连接释放；运行逻辑未改 |
| 类型、前端构建 | `out/performance-final-build.log` PASS |
| workspace 严格 Clippy、格式、CSS、设计、来源门禁、diff 空白检查 | PASS；严格 Clippy 见 `out/performance-delivery-clippy.log`，收尾门禁见 `out/performance-delivery-gates.log` |
| 原生构建 | PASS；`out/performance-delivery-native-build.log` |

复现入口：

```text
node tooling/scripts/generate-performance-plan.mjs
node tooling/scripts/native-live-e2e.mjs --plan out/native-live/performance-plan.json --provider-config <private-provider.json> --output <new-output-dir>
node tooling/scripts/native-live-e2e.mjs --plan tooling/native-live/performance-functional-plan.json --provider-config <private-provider.json> --output <new-output-dir>
```

已执行计划保持冻结；生成器后续将工具结果校验字段修正为 `outcomeStatus` /
`outcomeIsError`，未覆盖已执行计划的摘要。性能运行已另行审计实际 Journal 的 Read
request、成功 completion 和 transcript；最终功能计划使用正确字段严格检查三种工具。
凭据只放本机隔离配置；报告、堆、profile 和截图脱敏，不包含提供方密钥和服务地址。

截图：`performance-final/code-preview.png`、`performance-final/real-model-completed.png`、
`performance-functional-final/after-stop-and-recovery.png`，均在 `out/native-live/` 下。
原始测量、堆摘要、脱敏 `.heapsnapshot` 和 `.cpuprofile` 同目录保存，汇总见
`out/native-live/performance-e2e-summary.json`。

## 失败记录与版本绑定

| 保留的运行 | 结果 / 原因 |
| --- | --- |
| `performance-before` | 24 步后失败：实际旧打包契约不匹配，搜索零结果 |
| `performance-before-diagnostics` | 30 步后失败：诊断计划用追加空文本代替清空搜索；已修正夹具 |
| `performance-before-v2` / `v3` | 各 46 步后失败：旧流式页面 `Runtime.evaluate` / `Profiler.stop` 超时；实际 Read 成功，模型输出不同，保留证据 |
| `performance-after` | 58/58，但大流式内存样本和 profile 仍暴露内容 key 开销，继续修复 |
| `performance-functional` | 50 步后失败：测试选择器把“搜索 Ctrl+K”误写成精确“搜索”，已修正 |
| `performance-functional-v2` | 77 步后失败：等待误命中上一轮的文件名，新消息混入尚未清空的草稿；改为等待本轮 Journal 终态与空 Composer |
| `performance-functional-v3` | 88/88；随后补充停止后的 GC 对照成为最终 93/93 |

旧 EXE：`787739acae9a52b952b4c8c8a925973cdfefd05064a5f92cf677367466313a6b`。
最终 58 步性能 EXE：`0877eee8986dbbef40783387511f76845fa409f6c234ea56c2316c198c2e4c14`，
计划：`c6a967222e70e0db6b63f12e2deb18a3a8913f4a3b50eaae9dc87154bf8115c6`。
最后构建只加强 `cfg(test)` 通知断言，没有改变运行逻辑或前端产物；最终 93 步及日常窗口
使用 EXE：`21e3700e53059152cbb544cc42088401b31b1471158716655d1e8e6cf0941651`，
计划：`6ed1124212d7f08ccd7abdf52cff07462ee07f54a294176ec5a23de210507a00`。

前端来源仍为 ZCode 3.14.3 / `29628c9acdb81b703bbd4080c207a0e7ce5e276e`，来源仓库
工作区干净。最终来源绑定 `out/native-live/frontend-provenance-performance-delivery.json`：
来源 1550 文件、目标 1316，1135 一致 / 172 适配 / 243 裁剪 / 9 新增；目标树 SHA256
`00d5f4f0c3d9570a832b7043ecf8da0b17e9d50cf496de15c82a46d5ff75dd56`。
源码和旧二进制备份在 `out/native-live/performance-20261005-preimage/`；22 个受影响路径
摘要见 `performance-change-audit.json`，包含五个退役 Worker/全量搬运文件。

日常窗口沿用原数据根和 WebView2 profile，未使用目录选择器覆盖或 CDP 启动参数。
15 个原会话 metadata 的 SHA256 全部一致，provider、应用和前端配置摘要全部一致；
启动新增一个空会话，总数 15→16。见 `manual-performance-data-preservation.json`、
`manual-launch-performance-20261005.json`、`manual-performance-idle-20261005.json`。
未提交或推送 Git，保留原工作区的迁移和其他本地修改。

## 未验证范围与后续方向

- 本轮未重跑历史完整 25 scope；并行 workflow、浏览器原生表面、PTY 压力、中文 IME、
  RDP/休眠等保留先前各构建的证据，不能标为本轮通过。
- 短时间 GC 对照不能替代小时级曲线、跨多次切换的 retained dominator 分析，以及 Rust /
  WebView2 native heap、GPU 提交归因；剩余 RSS 高水位不等同 JavaScript 泄漏。
- 下一步优先冻结同一 Journal/回复进行同步布局 A/B，再做小时级多会话、预览和停止循环。
  时间线贴底、用户上滚、历史前插和冷恢复有竞态约束，不直接删除布局守卫来换取低采样。

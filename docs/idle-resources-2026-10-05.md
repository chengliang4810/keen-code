# 空闲 CPU 修复与内存观测

2026-10-04 至 2026-10-05 完成 CPU 口径修正、系统采样减负及 Rust 原生文件通知。
Windows 原生对照确认 CPU 实际下降；内存仍存在六百多 MiB 的聚合工作集，本次不宣称
已经消除内存问题。没有提交或推送 Git，没有修改 ZCode 源仓库；既有配置／会话均保留。

## 改动与决策

- `diagnostics/process_resources.rs`：宿主与受控 WebView2 的 CPU 时间增量除以整机
  逻辑处理器数。Windows 查询所有 processor group，当前机器为 20 个逻辑处理器。
  首个样本没有基线时保留未知。原 UI 的 13%–25% 单核口径约为整机 0.65%–1.25%。
- `frontend_rpc/desktop_controls.rs`：复用一个 `sysinfo::System`，仅按面板请求
  `refresh_memory()`；移除每次 `new_all()` 加 `refresh_all()` 的两轮全系统采集。
  可见面板继续使用原有串行请求、关闭取消和采样生命周期。
- `frontend_rpc/native_file_watcher.rs` 与 `services.rs`：移除 250ms 递归目录快照，
  新增固定 `notify = 8.2.0`，Windows 使用 `ReadDirectoryChangesW`。无变化时阻塞等待，
  不轮询、不依赖 Node。仅在实际事件到达时检查路径祖先；150ms 静默防抖、750ms 最长等待，
  多路径／溢出使用已有根目录刷新契约。订阅先校验归属、绑定消费者，再启动系统监听。
  最后一个订阅释放时撤销监听并等待分发退出；关闭描述拒绝迟到订阅，重订阅不会复用旧线程。
- 原 `file-watcher` 服务、事件字段、连接所有者和 UI DOM 均保留。新增依赖用于系统通知，
  lockfile 增加对应依赖及 `mio` 的 `log` feature；后续窗口卡死排查还固定了 Tao 上游
  修复源及其同源 macro，见下节，没有升级 Tauri、Wry 或其他既有 crate 版本。
- 原生验收入口增加资源采样时间戳、已脱敏 V8 堆证据及限定于合成项目的外部文件变化动作。
  原始堆快照不写磁盘，先解码字符串表再脱敏；128 MiB 上限防止证据无界增长。
  资源测量前后新增只读 `desktop_zoom_level` IPC 健康检查，防止 WebView 仍能渲染时
  把已经死锁的 Rust 宿主误判为空闲优化。

协议与生命周期见 [`protocols/frontend-rpc.md`](protocols/frontend-rpc.md)。原生后端选择可核对
[`notify` 官方文档](https://docs.rs/notify/8.2.0/notify/)。

## 追加发现：Windows 输入消息重入死锁

在完成 CPU 对照后，原配置窗口 PID 29768 失去 Windows 消息响应。重复的非侵入主线程
展开显示：`PeekMessageW` 在 Tao 键盘构建锁内同步进入窗口回调，内层再次获取同一锁，
停在 `WaitOnAddress`。实际有序栈见 `out/native-live/manual-window-unwind.json`；
没有原生文件通知／线程回收栈。WCT 只显示阻塞，并未单独识别循环。

这与 [Tao PR 1215](https://github.com/tauri-apps/tao/pull/1215) 的官方修复相符。
该修复发布于 0.36，当前 Tauri 2.11 约束 `tao ^0.35`，registry 尚无相应 patch。
本次根 Cargo patch 固定官方提交 `c704261c519c58cfdd0bc2d58ba24e06a0b71c92`，
crate 仍为 0.35.3；来源、许可证、Linux JIS 附带差异与后续移除条件见
[`../third-party/tao/SOURCE.md`](../third-party/tao/SOURCE.md)。

双线程原生探测同步发送 F24 键消息和焦点消息：旧 CPU 修复构建约 2 秒即永久无响应，
新构建 2400 条消息全部回复，最后 `WM_NULL` 仍能及时响应。
旧构建负例见 `out/native-live/keyboard-reentry-before-health/keyboard-reentry-probe.json`；
其资源计划因真实只读 IPC 超时而 FAIL，符合预期。最早探测版仅按进程筛选所有可见窗口，
被额外原生窗口挡住；修正为该进程的 `Tauri Window` 后复现。
另一次未包含宿主健康检查的旧计划虽报告 9 个步骤 PASS，但输入探测已确认卡死，
不能作为成功验收；两个原始报告均保留。

原配置 14 个会话确认 idle，正常关窗等待 4 秒未退出后，仅结束已确认锁死的本次宿主，
保留相同数据目录／WebView profile。14 份会话 metadata 的摘要均未变化。
这轮验证针对消息重入；真实中文 IME、休眠恢复与 RDP 场景仍需实机补验。

## CPU 面板开关对照

同一合成 Git 项目：206 个文件、约 340 个目录项，包括 `.git` 与被 Git 忽略的夹具。
两次均使用全新隔离数据和 WebView2 profile、1280×820 viewport、2× scale，WebView2
148.0.0.0。只有界面与文件动作，所有采样窗口均确认活跃会话为零。

以下是本次启动的进程树 CPU 时间增量除以墙钟时间及 20 个逻辑核，包含一个小型 conhost
辅助进程。修改前每阶段约 31 秒，修改后每阶段 7 个约 31 秒窗口，均为阶段加权平均；
不同运行的 RSS 受系统缓存、共享页与调试取证影响，不能把差值全部归因于代码。

| 面板状态 | 修改前整机 CPU | 修改后整机 CPU | 修改后聚合 RSS |
| --- | ---: | ---: | ---: |
| 关闭 | 0.6899% | 0.0508% | 667.8–687.0 MiB |
| 打开 | 1.1198% | 0.0902% | 668.2–676.7 MiB |
| 再关闭 | 0.6910% | 0.0166% | 672.5–678.5 MiB |

追加输入修复后的最终构建另做三阶段短回归，使用同一夹具、每阶段约 31 秒，均有前后
Rust IPC 健康回复：关闭 0.0401%、打开 0.1441%、再关闭 0.0177%。端点聚合 RSS
分别为 675.1／677.2／672.7 MiB；面板截图的即时显示为 716 MB。
本轮先发送了 2400 条输入／焦点消息，不能将其内存与上述长曲线直接等同比较。
证据为 `out/native-live/idle-resource-final/report.json` 及 `idle-resource-final-summary.json`。

原配置桌面另做 31.5 秒独立采样，PID 29768，整机 CPU 0.0297%；采样发生于后续输入
死锁之前，宿主各线程合计约
78ms CPU，没有此前持续递归扫描的热线程。该次 RSS 约 410 MiB，窗口／工作集回收状态
与隔离取证不同，不能据此承诺以后固定为 410 MiB。

证据：[`对照摘要`](../out/native-live/idle-resource-comparison.json)、
[`交互曲线`](../out/native-live/idle-resource-comparison.html)、
[`CSV`](../out/native-live/idle-resource-curve.csv)、
[`原配置独立采样`](../out/native-live/manual-idle-resources-after.json)。

## 内存与堆分析

修改后采样跨度 709846ms，约 11.8 分钟、21 个采样窗口。RSS 在 667.8–687.0 MiB 内
波动，结束约 672.8 MiB；没有该时段内单向持续上升的 RSS 曲线。私有提交从第一窗口的
555.6 MiB 到最后 570.7 MiB，面板再次关闭后的 7 个窗口为 570.4–571.3 MiB。
同 PID 对照显示，宿主从面板打开前到末尾增加约 1.0 MiB，其余约 24.4 MiB 变化主要在
一个 WebView2 进程。本次未保存该进程的角色及原生分配栈，不推断它是 renderer 或 GPU。

| 指标 | 起点 | 终点 |
| --- | ---: | ---: |
| `Runtime.getHeapUsage` GC 后 JavaScript usedSize | 26.477 MiB | 26.628 MiB |
| 堆快照节点 self_size 求和，含外部原生字节 | 58.935 MiB | 59.084 MiB |
| DOM 文档数 | 2 | 2 |
| DOM 节点数 | 638 | 615 |
| JavaScript 事件监听数 | 630 | 576 |

快照中数组缓冲原生字节约 16.48 MiB，外部字符串约 15.21 MiB，基本不变。
节点统计是 self size 分组，没有做完整 dominator/retained-size 分析。V8 堆、外部缓冲、
浏览器／GPU 原生分配与多个进程的共享映射不能互相等同；RSS 求和可能重复计入共享页。
堆快照和 GC 在 CPU 曲线两端采集，采集期间的开销不纳入上述 CPU 窗口。

已保存两次脱敏 `.heapsnapshot` 和 summary，位于
[`修改后证据目录`](../out/native-live/idle-resources-after/report.json)。小时／天级趋势、
多轮开关与真实长对话后的回收、WebView2 原生堆／GPU 归因仍为 pending；不能用本次
11.8 分钟观测证明没有慢速泄漏。

## 验证记录

| 检查 | 实际结果与日志 |
| --- | --- |
| Rust workspace 全目标 | 55 suites，3141 PASS，0 FAIL，8 ignored；`out/idle-resource-fix-workspace-tests.log` |
| Desktop 范围 | 1150 PASS，5 ignored；含 7 个原生文件通知测试、CPU 归一化及既有 RPC 隔离／订阅测试 |
| 工作区格式 | `cargo fmt --all -- --check` PASS；`out/idle-resource-fix-format.log` |
| 严格 Clippy | `cargo clippy --workspace --all-targets --locked -- -D warnings` PASS；`out/idle-resource-fix-clippy-final.log` |
| 前端测试 | 28 files／90 tests PASS；`out/idle-resource-fix-ui-tests.log` |
| 脚本测试 | 76 PASS，1 macOS skip；`out/idle-resource-fix-script-tests.log` |
| 类型／CSS | `pnpm run typecheck`、`pnpm run lint:css` PASS；见对应 `idle-resource-fix-*.log` |
| 设计／来源 | `pnpm run check:design-system`、`pnpm run check:clean-room` PASS；ZCode HEAD 固定且工作区干净 |
| Windows 构建 | `cargo build -p keencode-desktop --features native-desktop-tests --locked` PASS；`out/idle-resource-fix-build.log` |
| 修改前原生对照 | `out/native-live/idle-resources-before/report.json`：19/19 PASS |
| 修改后资源步骤 | `out/native-live/idle-resources-after/report.json`：前 40 个资源步骤 PASS，21 窗口及 2 次堆取证齐全；整个计划仍为 FAIL，见下述原因 |
| 原生通知补验 | `out/native-live/idle-resource-native-events/report.json`：29/29 PASS，Git HEAD、文件新建／改名／删除、页面重载后重新订阅均成功 |
| 追加输入修复后的桌面单测／Clippy | 1150 PASS、5 ignored，workspace 严格 Clippy PASS；`out/idle-resource-fix-tao-desktop-tests.log`、`out/idle-resource-fix-tao-clippy.log` |
| 最终构建／格式 | locked native build、workspace fmt PASS；`out/idle-resource-fix-tao-build.log`、`out/idle-resource-fix-tao-format.log`。MSVC 正常创建库提示被 Rust 记作一条 linker_messages warning，构建成功 |
| 最终原生回归 | `out/native-live/idle-resource-final/report.json`：42/42 PASS，2400 条输入／焦点消息、6 次真实只读宿主 IPC、3 阶段 CPU、目录／Git 事件与重订阅均通过 |
| 最终脚本／凭据扫描 | 76 PASS、1 skip；47 份证据、449321523 bytes，私有字段扫描 PASS；`out/idle-resource-fix-scripts-final.log`、`out/idle-resource-fix-secret-scan.log` |
| 原配置恢复 | `out/native-live/manual-launch-idle-resources-20261005.json`：PID 32040，保留原数据／WebView profile，原生目录选择器覆盖关闭；最终健康采样另见 `manual-idle-resources-final.json` |

最终原配置桌面独立采样 30.2 秒：整机 CPU 0.0155%，聚合 RSS 644.7 MiB、私有提交
488.2 MiB，采样前后窗口均响应。原 14 份 metadata 仍存在且摘要未变；启动后出现一个
新的空闲“新对话”，当前共 15 个会话、活跃任务为零。保存了后代角色：browser、renderer、
GPU、两个 utility、crashpad，加宿主与 conhost；没有保存原生分配栈，不能据角色列表
判断泄漏或可回收上限。

该次宿主 RSS 约 116.7 MiB、私有提交 46.0 MiB；WebView2 renderer 为 168.8／200.1 MiB，
browser 为 169.1／79.9 MiB，GPU 为 109.4／134.9 MiB。其余是 utility、crashpad 与 conhost。
这说明这份样本的大部分占用位于 WebView2 进程，不能仅从 RSS 推断它们存在泄漏。

长采样计划完成资源取证后，Git HEAD 刷新断言只等待 30 秒而失败。现有 UI 的
`useGitAutoRefresh` 使用 60 秒防抖，补验调整为 90 秒等待后，事件链路全部通过；
未改动该既有 UI 设置，未覆盖首轮失败报告。补验在页面重载时保留一个 Tauri 旧异步
callback 不存在的 warning，已从新连接继续接收事件；`frontendFaults` 和 `protocolFaults`
均为空，不能把 warning 数量写成零。最终 42 步回归同样保留页面重载时的一条旧 callback
warning，没有未处理前端异常或协议故障。

全工作区编译首轮因 D 盘耗尽中止。核对原配置 14 个会话空闲后正常关闭桌面，使用
`cargo clean -p keencode-desktop` 清除约 109.1 GiB 可生成构建产物，再成功重建及重测。
一次手写递归缓存删除命令被自动执行策略拒绝，未执行；随后使用上述 Cargo 官方清理
命令完成。源码、配置、会话、备份与验收证据均保留。Clippy 首轮发现测试代码多余借用，
已修正并通过最终严格检查。没有更改产品前端源码，没有重跑完整历史功能及真实模型请求；
本次原生项目的模型网络记录不存在，不能算作模型测试。

## 版本与复现

CPU／堆长曲线测量构建为 110426112 bytes，SHA256：
`e4124fbb5f84df33d2125ba2adae4ea4d3c3100907cb7fc5c4ec9fe8e1768a5e`。
追加输入修复后的交付 EXE 为 `target/debug/keencode-desktop.exe`，110407680 bytes，SHA256：
`787739acae9a52b952b4c8c8a925973cdfefd05064a5f92cf677367466313a6b`。
修改前 EXE SHA256：
`4102b31860fdeff1a28623dba1ea0e2c664cd8cb98eef32683d9aa4f716f7e9c`。
受影响源码和旧 EXE 备份在 `out/native-live/idle-resource-fix-preimage/`，本轮源码差异见
`out/idle-resource-fix-source.patch`（根 Cargo.toml 的新增 patch 块以前后已知差异重建
基线，其他旧文件使用修改前备份）；现有工作区的大量迁移修改未重置。

原生通知补验命令：

```text
node tooling/scripts/native-live-e2e.mjs --plan out/native-live/idle-resource-events-plan.json --provider-config out/native-live/native-provider-6f2.json --output out/native-live/idle-resource-native-events --port 9243
```

provider 配置是现有本地私有文件，凭据不写入命令、源码、截图、图表或报告。
原始执行计划冻结在各证据目录的 `plan-snapshot.json`，报告绑定 plan 与 EXE 摘要。
截图为 `idle-resources-after/cpu-after.png`、`memory-after.png` 及
`idle-resource-native-events/native-file-events.png`，已复核真实 ResourceManager 与文件树。
最终构建截图另为 `idle-resource-final/final-cpu.png`、`final-memory.png` 和
`native-file-events.png`，三份均已复核。

建议下一轮专门做小时级运行和重复面板开关，并按 WebView2 角色采集私有提交与原生分配，
再据增长归因决定是否调整资源加载或缓存。macOS／Linux 原生后端未实机验证。

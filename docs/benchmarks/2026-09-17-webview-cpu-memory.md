# WebView CPU 与内存：多会话流式负载定位与优化

日期：2026-09-17。平台：macOS 14.8.7，Intel i7-9750H 2.60GHz（12 逻辑核），16 GiB RAM；代码为当前工作区未提交改动，含当次修复。测量方式：`sample`（1ms 采样）+ `ps` CPU 时间增量（长窗口）+ `footprint`（物理内存，活动监视器同口径）。

## 症状

多个对话并行流式输出时，`tauri://localhost`（WebContent）占用约 91%–121% 单核，`keencode-desktop`（Rust 原生）9%–34%；活动监视器显示原生进程线程数 40–50。

## 根因与修复

| # | 根因 | 位置 | 修复 |
|---|---|---|---|
| 1 | 流空闲看门狗在每次 `Poll::Ready` 丢弃计时器，下个 `Poll::Pending` 重新 `spawn` OS 线程：每 token 创建一个线程，实测 10 秒窗口 2831 个 `keencode-stream-watchdog`（约 283/秒） | `crates/keencode-provider/src/client.rs` | `DeadlineTimer` 改为共享可推进到期槽位，事件到达只 `defer` 不重建；线程数与并发流同阶 |
| 2 | 每条 user 消息渲染新建 2 个 `Intl.DateTimeFormat`；`formatMetricTokens` 每次 `toLocaleString` 新建 `NumberFormat` | `src/lib/messageTime.ts`、`src/lib/turnMetricsPresentation.ts` | 模块级按语言缓存格式化器 |
| 3 | 消息循环对每条工具行调用 `isToolInlinedInAssistants`（嵌套全量扫描），O(n²) | `src/lib/session.ts`、`ConversationThread.tsx` | 一次扫描建 `inlinedToolCallIds` 集合，行渲染 O(1) 查询 |
| 4 | 后台会话每个流式分片都 `commitWorkspace` + `setLiveMap`，即使内容未变也生成新对象触发全树重渲染 | `src/hooks/acp-runtime/events.ts`、`src/lib/sessionLiveStore.ts` | 投影仅在待投影集含可见会话时提交；live 快照内容未变（除 `updatedAt`）时复用原对象 |
| 5 | 每帧对全部历史消息重新投影：解析附件、合并、复制 segments。实测 13MB 历史（300 条消息）每帧 39.5ms、产生约 4MB 垃圾（60 帧 238.9MB） | `src/lib/sessionProjection.ts` | `projectAcpHistory` 按历史数组身份（WeakMap）+ 长度 + Session 缓存投影结果；store 对 history 只 push 或整体替换、元素不可变，push 使长度变化自动失效，数组替换产生新键由 WeakMap 回收 |

## 结果

看门狗线程（10 秒 `sample` 窗口内唯一线程数）：

| 版本 | 线程数 |
|---|---|
| 修复前 | 2831 |
| 修复后 | 2–6 |

WebContent 主线程（5 秒采样，流式期间）：忙样本占比 43%–45% → **18%**；ICU 热点（`numberProtoFuncToLocaleString` 92、`initializeNumberFormat` 48、`canonicalizeLocaleList` 10、`unumf_openForSkeleton` 14）全部归零；剩余忙时主要是布局与合成（`RenderBlock::layout`、`RenderLayerCompositor`），属真实绘制。

整机 CPU（`ps` 时间增量，长窗口）：

| 场景 | 原生 | 前端 | GPU | 合计（单核） |
|---|---|---|---|---|
| 1 个对话流式（60s 干净窗口） | 19.6% | 13.0% | 1.8% | 34.4% |
| 2 个对话（157–180s 窗口） | 32.0–32.2% | 27.4–36.2% | 4.2–4.4% | 64–73% |

历史投影缓存探针（300 条消息 / 约 2250 段 / 13MB 文本，60 帧）：

| 指标 | 优化前 | 优化后 |
|---|---|---|
| `projectAcpHistory` 每帧 | 19.87ms | 0.00ms |
| `projectAcpConversation` 每帧 | 39.49ms | 0.03ms |
| 堆增量（60 帧） | 238.9MB | −0.1MB |

## 内存基线

物理 footprint（活动监视器口径），1–2 个活跃对话：

| 进程 | 当前 | 峰值 |
|---|---|---|
| keencode-desktop（原生） | 112–168 MB | 145–218 MB |
| WebContent（前端） | 242–523 MB | 593–765 MB |
| GPU 进程 | 38–157 MB | 373–460 MB |
| Networking | ≈3 MB | ≈6 MB |
| **合计** | **约 610–731 MB** | |

157 秒趋势稳定（原生 119→126 MB、前端 285→287 MB），无泄漏；前端瞬时波动来自虚拟列表与 markdown 缓存。WebContent 堆构成（`heap`）：non-object（WebKit Malloc）438MB 为主，`JSC::StringSourceProvider` 26117 个/128MB，其余为 JSC JIT 代码与 CoreFoundation 对象。

原生进程 CPU（19.6%–32.2%）99.3% 线程样本为等待/睡眠，消耗来自真实系统调用：`__fcntl`（journal `F_FULLFSYNC` 批量刷盘，默认 64 条或 100ms）、工具执行 `__fork`、`__getdirentries64`。属负载成本而非缺陷。

## 后续候选（未实施）

- 后台会话保留：单次运行加载 30 个会话后，`acpWorkspaceRef.sessions` 与 `messagesBySessionRef` 无淘汰。LRU 淘汰可释放数百 MB，但切回需重新恢复（首次加载 `readyMs` 实测 98–1950ms），且需清 `replay.loaded` 让 `replayHistory` 走 `recoverSession`；涉及切回体验变化，未在本次实施。
- journal 批量刷盘参数（64 条 / 100ms）与持久性语义耦合，未调整。

## 局限

修复前数据采集自 4 个并发对话，修复后为 1–2 个，CPU 百分比不直接可比；线程数与热点构成不受影响。`phys_footprint_peak` 为进程生命周期峰值，含启动瞬时。全部数据为本机开发机实测，未在基准设备矩阵复现。

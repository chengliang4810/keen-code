# 前端流式投影性能维护报告

日期：2026-09-21

## 1. 结论

本次维护收敛了前端 ACP 流式消息从投递、归约到界面投影的性能热点，并保留了原有消息内容、思考阶段、工具段和 Turn 终态语义。

- 实时正文和思考字段改为按 `MessageSegment[]` 身份缓存。追加 delta 时只归约新增字符或新增段，非追加变化则回退全文重建。
- 工具段按 `toolCallId` 建立索引，避免工具密集时间线中的重复线性查找。
- 页面级 Long Task 按同一 `performance.now()` 时间轴归属唯一 Turn，不再广播计入所有活跃会话。
- 新增 JSON ACP 投递到解析器、Reducer 和 UI 投影的跨层回归测试。

固定压力样本的 7 次运行中，未缓存字段归约中位数为 `33.8 ms`，增量缓存路径中位数为 `26.8 ms`，按中位数计算下降约 `20.7%`。这是当前本地运行环境和固定夹具下的观测值，不是所有设备或真实模型流量下的固定收益承诺。

## 2. 基线与热点

原实时投影路径在每次刷新时对当前段集合重新执行工具压缩、字段归约和正文拼接。长文本流式输出中，正文已增长的字符会被重复扫描。

同时，`compactMessageSegments` 原先使用 `findIndex` 查找同一 `toolCallId`，工具段较多时会形成 O(n²) 的合并查找。前端性能采样中的 Long Task 观察器也把每条页面级任务写入所有活跃 Turn，多个并发 Session 会重复统计同一页面阻塞。

## 3. 修改内容

### `src/lib/sessionProjection.ts`

- 新增基于 `WeakMap<MessageSegment[], LiveTextProjectionCache>` 的实时正文和思考阶段缓存。
- 对 ACP reducer 的 append-only 流式段执行增量归约：尾段增长只追加新增字符，新段只处理新增范围。
- 检测到段数组截断、最新文本段被替换或缩短时，回退到全文重建，避免复用失效正文。
- 保留既有 100 ms 合并、历史投影缓存、虚拟列表和增量 Markdown 行为。

### `src/lib/session.ts`

- `compactMessageSegments` 使用 `Map<string, number>` 记录工具段索引，将同一工具调用的合并定位从 O(n²) 降为 O(n)。

### `src/lib/frontendPerformance.ts`

- 为 Turn 保存结束时间，Long Task 只归属时间窗口内最近开始且覆盖该任务的唯一 Turn。
- 暴露测试注入入口，并在测试重置时断开 `PerformanceObserver`，避免跨用例残留。

### 测试

- `src/lib/sessionProjection.test.ts` 覆盖尾段正文追加、思考阶段追加和非 append 缩减回退。
- `src/lib/sessionProjection.performance.test.ts` 固定 6000 个 delta、每 10 个 delta 投影一次，记录 600 次投影和 384000 个正文字符，并对比全量字段归约与增量路径。
- `src/lib/frontendPerformance.test.ts` 覆盖并发 Turn 下的 Long Task 唯一归属。
- `src/lib/acp/streamingPipeline.test.ts` 覆盖 JSON Tauri 投递、严格 ACP 解析、Reducer、生命周期终态和文本顺序。权威 `turn_started`/`turn_completed` 夹具保留严格要求的 `journalSequence`。

## 4. 性能样本

测试命令：

```powershell
pnpm.cmd exec vitest run src/lib/sessionProjection.performance.test.ts
```

样本参数：

- delta 数量：`6000`
- 每次投影间隔：`10` 个 delta
- 投影次数：`600`
- 正文字符数：`384000`
- 独立运行次数：`7`

结果摘要：

| 指标 | 中位数 | 观测范围 |
| --- | ---: | ---: |
| 100 ms 风格完整投影压力 | `33.9 ms` | `32.6–34.6 ms` |
| 未缓存字段归约 | `33.8 ms` | `32.9–35.2 ms` |
| 增量缓存字段归约 | `26.8 ms` | `26.0–27.3 ms` |
| 单次日志中的下降比例 | `20.1%` | `18.1%–25.5%` |

完整投影压力测试只用于保持内容顺序和记录比较样本，不设置依赖机器速度的硬性能阈值。

## 5. 验证结果

- 相关测试：4 个文件、68 项通过。
- `pnpm.cmd run typecheck`：通过。
- `pnpm.cmd run lint:css`：通过。
- `git diff --check`：通过；仅有工作树既有的换行格式提示。
- 完整 `pnpm.cmd exec vitest run`：151 个测试文件、1482 项中，149 个文件和 1479 项通过。
- 完整 Vitest 剩余 3 项失败集中在未修改的现有 UI 契约：`src/App.contract.test.ts` 的浮层源码边界断言 1 项，以及 `src/components/ui/button.test.tsx` 的共享 Button 包装/尺寸断言 2 项；与本次流式投影、Session 压缩和前端性能采样改动无直接关系。

## 6. 未验证范围与风险

- 尚未启动原生 Tauri，也未在 Windows WebView2 中验证真实 ACP 事件到 DOM 更新的端到端延迟、滚动跟随、Markdown 渲染和虚拟列表行为。
- 性能数据来自 Node/Vitest 固定夹具，不等同于真实模型网络、工具调用、Markdown 解析和桌面窗口负载下的 CPU/RSS/P95 结果。
- 增量缓存依赖 ACP reducer 对实时文本段的 append-only 约束；对最新段缩短、段截断或段替换会主动全文重建。若未来允许修改历史段内容，应同步引入显式 revision 或扩大缓存失效条件。

建议后续在原生桌面验收中补采首 token、事件到 DOM 更新的中位数/P95、Long Task、活跃 CPU 和 RSS，并单独处理当前已有的 3 项 UI 契约测试失败。

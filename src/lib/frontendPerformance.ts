import { performanceRecord } from "@/lib/acp/api";
import { observabilityRecordMetric } from "@/lib/observability";

type Aggregate = {
  count: number;
  totalMs: number;
  maxMs: number;
};

type TurnPerformance = {
  sessionId: string;
  turnId: string;
  startedAt: number;
  completedAt: number | null;
  deliveryCount: number;
  projection: Aggregate;
  markdown: Aggregate & {
    sourceChars: number;
    maxSourceChars: number;
    fullParseCount: number;
  };
  virtualizer: Aggregate & { measurementCount: number };
  longTasks: Aggregate;
  flushTimer: ReturnType<typeof setTimeout> | null;
};

const turns = new Map<string, TurnPerformance>();
const activeTurnBySession = new Map<string, string>();
/** 观测不能成为长时间运行页面的第二份会话状态。 */
const MAX_TRACKED_TURNS = 256;
let longTaskObserver: PerformanceObserver | null = null;

function aggregate(): Aggregate {
  return { count: 0, totalMs: 0, maxMs: 0 };
}

function add(target: Aggregate, durationMs: number): void {
  if (!Number.isFinite(durationMs) || durationMs < 0) return;
  target.count += 1;
  target.totalMs += durationMs;
  target.maxMs = Math.max(target.maxMs, durationMs);
}

function activeTurn(sessionId: string | number | null | undefined): TurnPerformance | null {
  if (typeof sessionId !== "string") return null;
  const turnId = activeTurnBySession.get(sessionId);
  return turnId ? turns.get(turnId) ?? null : null;
}

function ensureLongTaskObserver(): void {
  if (longTaskObserver || typeof PerformanceObserver === "undefined") return;
  const supported = PerformanceObserver.supportedEntryTypes?.includes("longtask") ?? false;
  if (!supported) return;
  longTaskObserver = new PerformanceObserver((list) => {
    for (const entry of list.getEntries()) {
      recordLongTaskEntry(entry);
    }
  });
  longTaskObserver.observe({ entryTypes: ["longtask"] });
}

/**
 * Long Task 是页面级 PerformanceEntry，不携带 Session/Turn 标识；只按同一
 * `performance.now()` 时间轴把它归给一个拥有者。不能把一条页面阻塞广播给
 * 所有活跃会话，否则并发 Session 会重复计算前端卡顿。
 */
function recordLongTaskEntry(entry: Pick<PerformanceEntry, "startTime" | "duration">): void {
  if (!Number.isFinite(entry.startTime) || !Number.isFinite(entry.duration) || entry.duration < 0) return;
  const start = entry.startTime;
  const end = start + entry.duration;
  const candidates = [...turns.values()].filter((turn) =>
    turn.startedAt <= end && (turn.completedAt === null || start <= turn.completedAt),
  );
  if (!candidates.length) return;
  const started = candidates.filter((turn) => turn.startedAt <= start);
  const owner = (started.length ? started : candidates).sort((left, right) => {
    if (left.startedAt !== right.startedAt) return right.startedAt - left.startedAt;
    return left.turnId.localeCompare(right.turnId);
  })[0];
  if (owner) add(owner.longTasks, entry.duration);
}

/** 测试和原生视觉夹具可注入同一时间域的 Long Task，生产 Observer 走同一归属逻辑。 */
export function recordFrontendLongTask(startTime: number, duration: number): void {
  recordLongTaskEntry({ startTime, duration });
}

export function beginFrontendTurnPerformance(sessionId: string, turnId: string): void {
  const previousTurnId = activeTurnBySession.get(sessionId);
  if (previousTurnId && previousTurnId !== turnId) {
    const previous = turns.get(previousTurnId);
    // 已完成 Turn 仍可能处于一秒 flush 窗口，必须保留其聚合结果；
    // 只有断线/切换时仍未完成的旧 Turn 才能直接释放。
    if (previous?.completedAt === null || previous === undefined) {
      if (previous?.flushTimer) clearTimeout(previous.flushTimer);
      turns.delete(previousTurnId);
    }
  }
  activeTurnBySession.set(sessionId, turnId);
  if (!turns.has(turnId)) {
    turns.set(turnId, {
      sessionId,
      turnId,
      startedAt: performance.now(),
      completedAt: null,
      deliveryCount: 0,
      projection: aggregate(),
      markdown: { ...aggregate(), sourceChars: 0, maxSourceChars: 0, fullParseCount: 0 },
      virtualizer: { ...aggregate(), measurementCount: 0 },
      longTasks: aggregate(),
      flushTimer: null,
    });
  }
  // Session 断线或宿主卸载时可能没有终态事件，保留窗口也不能无限增长。
  while (turns.size > MAX_TRACKED_TURNS) {
    const removable = [...turns.entries()].find(([, turn]) => turn.completedAt !== null);
    const key = removable?.[0] ?? turns.keys().next().value;
    if (key === undefined) break;
    const turn = turns.get(key);
    if (turn?.flushTimer) clearTimeout(turn.flushTimer);
    turns.delete(key);
    if (turn && activeTurnBySession.get(turn.sessionId) === key) {
      activeTurnBySession.delete(turn.sessionId);
    }
  }
  ensureLongTaskObserver();
}

/** 连接断开、恢复失败或组件卸载时主动释放当前轮观测。 */
export function abandonFrontendTurnPerformance(sessionId: string, turnId?: string): void {
  const activeId = activeTurnBySession.get(sessionId);
  const targetId = turnId ?? activeId;
  if (!targetId) return;
  const turn = turns.get(targetId);
  if (turn?.flushTimer) clearTimeout(turn.flushTimer);
  turns.delete(targetId);
  if (activeId === targetId) activeTurnBySession.delete(sessionId);
}

export function recordFrontendDelivery(sessionId: string): void {
  const turn = activeTurn(sessionId);
  if (turn) turn.deliveryCount += 1;
}

export function recordFrontendProjection(sessionId: string, durationMs: number): void {
  const turn = activeTurn(sessionId);
  if (turn) add(turn.projection, durationMs);
}

export function recordMarkdownParse(input: {
  turnId?: string;
  durationMs: number;
  sourceChars: number;
  fullParse: boolean;
}): void {
  if (!input.turnId) return;
  const turn = turns.get(input.turnId);
  if (!turn) return;
  add(turn.markdown, input.durationMs);
  turn.markdown.sourceChars += input.sourceChars;
  turn.markdown.maxSourceChars = Math.max(turn.markdown.maxSourceChars, input.sourceChars);
  if (input.fullParse) turn.markdown.fullParseCount += 1;
}

export function recordVirtualizerWork(
  sessionId: string | number | null | undefined,
  durationMs: number,
  measurement = false,
): void {
  const turn = activeTurn(sessionId);
  if (!turn) return;
  add(turn.virtualizer, durationMs);
  if (measurement) turn.virtualizer.measurementCount += 1;
}

function rounded(value: number): number {
  return Math.round(value * 10) / 10;
}

export function completeFrontendTurnPerformance(sessionId: string, turnId: string): void {
  const turn = turns.get(turnId);
  if (!turn || turn.sessionId !== sessionId || turn.flushTimer) return;
  turn.completedAt = performance.now();
  // Final Markdown settlement and ResizeObserver callbacks occur after the terminal delivery.
  turn.flushTimer = setTimeout(() => {
    turns.delete(turnId);
    if (activeTurnBySession.get(sessionId) === turnId) {
      activeTurnBySession.delete(sessionId);
    }
    const payload = {
      schema: 1,
      sessionId,
      turnId,
      elapsedMs: Math.round(performance.now() - turn.startedAt),
      deliveryCount: turn.deliveryCount,
      projection: {
        count: turn.projection.count,
        totalMs: rounded(turn.projection.totalMs),
        maxMs: rounded(turn.projection.maxMs),
      },
      markdown: {
        count: turn.markdown.count,
        totalMs: rounded(turn.markdown.totalMs),
        maxMs: rounded(turn.markdown.maxMs),
        sourceChars: turn.markdown.sourceChars,
        maxSourceChars: turn.markdown.maxSourceChars,
        fullParseCount: turn.markdown.fullParseCount,
      },
      virtualizer: {
        count: turn.virtualizer.count,
        totalMs: rounded(turn.virtualizer.totalMs),
        maxMs: rounded(turn.virtualizer.maxMs),
        measurementCount: turn.virtualizer.measurementCount,
      },
      longTasks: {
        count: turn.longTasks.count,
        totalMs: rounded(turn.longTasks.totalMs),
        maxMs: rounded(turn.longTasks.maxMs),
      },
    };
    void performanceRecord("frontend.turn_performance", JSON.stringify(payload)).catch(() => {});
    // 结构化指标与兼容日志并行保留：日志供历史诊断使用，指标供运行时面板查询。
    void Promise.all([
      observabilityRecordMetric("frontend.turn.elapsed_ms", payload.elapsedMs, "ms"),
      observabilityRecordMetric("frontend.turn.delivery_count", payload.deliveryCount, "count"),
      observabilityRecordMetric("frontend.turn.projection_total_ms", payload.projection.totalMs, "ms"),
      observabilityRecordMetric("frontend.turn.projection_max_ms", payload.projection.maxMs, "ms"),
      observabilityRecordMetric("frontend.turn.markdown_total_ms", payload.markdown.totalMs, "ms"),
      observabilityRecordMetric("frontend.turn.markdown_full_parse_count", payload.markdown.fullParseCount, "count"),
      observabilityRecordMetric("frontend.turn.virtualizer_total_ms", payload.virtualizer.totalMs, "ms"),
      observabilityRecordMetric("frontend.turn.virtualizer_measurement_count", payload.virtualizer.measurementCount, "count"),
      observabilityRecordMetric("frontend.turn.long_task_count", payload.longTasks.count, "count"),
    ]).catch(() => {});
  }, 1_000);
}

export function resetFrontendPerformanceForTests(): void {
  longTaskObserver?.disconnect();
  longTaskObserver = null;
  for (const turn of turns.values()) {
    if (turn.flushTimer) clearTimeout(turn.flushTimer);
  }
  turns.clear();
  activeTurnBySession.clear();
}

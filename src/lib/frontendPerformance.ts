import { performanceRecord } from "@/lib/acp/api";

type Aggregate = {
  count: number;
  totalMs: number;
  maxMs: number;
};

type TurnPerformance = {
  sessionId: string;
  turnId: string;
  startedAt: number;
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
      for (const turn of turns.values()) add(turn.longTasks, entry.duration);
    }
  });
  longTaskObserver.observe({ entryTypes: ["longtask"] });
}

export function beginFrontendTurnPerformance(sessionId: string, turnId: string): void {
  const previousTurnId = activeTurnBySession.get(sessionId);
  if (previousTurnId && previousTurnId !== turnId) turns.delete(previousTurnId);
  activeTurnBySession.set(sessionId, turnId);
  if (!turns.has(turnId)) {
    turns.set(turnId, {
      sessionId,
      turnId,
      startedAt: performance.now(),
      deliveryCount: 0,
      projection: aggregate(),
      markdown: { ...aggregate(), sourceChars: 0, maxSourceChars: 0, fullParseCount: 0 },
      virtualizer: { ...aggregate(), measurementCount: 0 },
      longTasks: aggregate(),
      flushTimer: null,
    });
  }
  ensureLongTaskObserver();
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
  }, 1_000);
}

export function resetFrontendPerformanceForTests(): void {
  for (const turn of turns.values()) {
    if (turn.flushTimer) clearTimeout(turn.flushTimer);
  }
  turns.clear();
  activeTurnBySession.clear();
}

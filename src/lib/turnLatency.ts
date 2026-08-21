import type { UsageUpdateEvent } from "./acp/events";

/**
 * 单轮响应观测的纯数据模型。
 *
 * 所有时间点必须来自同一单调时钟。浏览器侧应使用
 * `turnLatencyNow()`，不要把 `Date.now()` 与 `performance.now()` 混用。
 * `null` 始终表示该信号尚未被观测到，不能当作 0 毫秒。
 */

export interface TurnTokenUsageObservation {
  /** 同一轮内稳定且唯一；优先使用 provider requestId。 */
  readonly observationId: string;
  /** Provider 报告的输入 Token 总数，包含缓存读取 Token。 */
  readonly inputTokens: number;
  /** Provider 明确报告的缓存读取 Token；未报告时为 null。 */
  readonly cacheReadTokens: number | null;
  /** Provider 明确报告的缓存创建 Token；未报告时为 null。 */
  readonly cacheCreationTokens: number | null;
}

export interface TurnLatencyState {
  readonly turnId: string;
  /** 用户触发发送时的单调、Epoch 对齐时间点（毫秒）。 */
  readonly startedAtMs: number;
  /** Host 已接受本轮消息；不是等待完整 sessionSend 返回。 */
  readonly sendAcknowledgedAtMs: number | null;
  /** Provider 流的首个 SSE/流事件到达 Host。 */
  readonly firstSseAtMs: number | null;
  /** 首段主 Agent reasoning 或正文已经提交到界面 DOM。 */
  readonly firstVisibleTokenAtMs: number | null;
  /** Agent 本轮结束信号到达。 */
  readonly completedAtMs: number | null;
  /** 按 LLM 请求去重后的本轮用量；一个 Agent Turn 可包含多次请求。 */
  readonly usageObservations: readonly TurnTokenUsageObservation[];
}

export interface TurnLatencySummary {
  readonly turnId: string;
  /** 发送到 Host 接受消息的耗时。 */
  readonly sendAcknowledgementMs: number | null;
  /** 发送到 Provider 首个 SSE/流事件的耗时。 */
  readonly timeToFirstSseMs: number | null;
  /** 发送到首段 reasoning/正文提交到界面 DOM 的耗时。 */
  readonly timeToFirstVisibleTokenMs: number | null;
  /** 发送到 Agent 本轮结束的耗时。 */
  readonly totalMs: number | null;
  /** 本轮全部 LLM 请求的输入 Token；尚无用量事件时为 null。 */
  readonly inputTokens: number | null;
  /** 全部请求都明确报告缓存读取量时为聚合值，否则为 null。 */
  readonly cacheReadTokens: number | null;
  /** 全部请求都明确报告缓存创建量时为聚合值，否则为 null。 */
  readonly cacheCreationTokens: number | null;
}

interface TurnActionBase {
  /** 防止旧回合的异步事件污染同一 Session 的新回合。 */
  readonly turnId: string;
}

export type TurnLatencyAction =
  | (TurnActionBase & {
      readonly type: "send_acknowledged";
      readonly atMs: number;
    })
  | (TurnActionBase & {
      readonly type: "first_sse";
      readonly atMs: number;
    })
  | (TurnActionBase & {
      readonly type: "first_visible_token";
      readonly atMs: number;
    })
  | (TurnActionBase & {
      readonly type: "completed";
      readonly atMs: number;
    })
  | (TurnActionBase & {
      readonly type: "usage_observed";
      readonly observationId: string;
      readonly inputTokens: number;
      readonly cacheReadTokens?: number | null;
      readonly cacheCreationTokens?: number | null;
    });

/**
 * 返回 Epoch 对齐的单调时间点，便于日志排查又不受系统时钟回拨影响。
 * Tauri WebView 和现代浏览器都提供 performance.timeOrigin。
 */
export function turnLatencyNow(): number {
  if (
    typeof performance !== "undefined" &&
    Number.isFinite(performance.timeOrigin) &&
    Number.isFinite(performance.now())
  ) {
    return performance.timeOrigin + performance.now();
  }
  return Date.now();
}

/** 创建一个尚未观测到 Host/Provider/UI 信号的新回合。 */
export function createTurnLatencyState(
  turnId: string,
  startedAtMs: number,
): TurnLatencyState {
  const normalizedTurnId = turnId.trim();
  if (!normalizedTurnId) {
    throw new TypeError("turnId must not be empty");
  }
  if (!isTimestamp(startedAtMs)) {
    throw new RangeError("startedAtMs must be a finite non-negative number");
  }
  return {
    turnId: normalizedTurnId,
    startedAtMs,
    sendAcknowledgedAtMs: null,
    firstSseAtMs: null,
    firstVisibleTokenAtMs: null,
    completedAtMs: null,
    usageObservations: [],
  };
}

function isTimestamp(value: number): boolean {
  return Number.isFinite(value) && value >= 0;
}

function tokenCount(value: number | null | undefined): number | null {
  if (value == null || !Number.isFinite(value) || value < 0) return null;
  return Math.floor(value);
}

type MilestoneField =
  | "sendAcknowledgedAtMs"
  | "firstSseAtMs"
  | "firstVisibleTokenAtMs"
  | "completedAtMs";

/**
 * 首次写入获胜；重复通知不会改变已记录的真实首个时间点。
 * 各通道可能乱序投递，因此只夹到本轮起点，不按 reducer 到达顺序篡改来源时间。
 */
function recordMilestone(
  state: TurnLatencyState,
  field: MilestoneField,
  atMs: number,
): TurnLatencyState {
  if (state[field] != null || !isTimestamp(atMs)) return state;
  const monotonicAtMs = Math.max(state.startedAtMs, atMs);
  return {
    ...state,
    [field]: monotonicAtMs,
  };
}

function maxObservedCount(
  previous: number | null,
  next: number | null | undefined,
): number | null {
  const normalized = tokenCount(next);
  if (normalized == null) return previous;
  return previous == null ? normalized : Math.max(previous, normalized);
}

/**
 * 同 observationId 的增量/重复用量只更新同一项，且 Token 计数不倒退。
 * 这既允许 Provider 后补缓存字段，也避免重复 session/update 被重复求和。
 */
function recordUsage(
  state: TurnLatencyState,
  action: Extract<TurnLatencyAction, { type: "usage_observed" }>,
): TurnLatencyState {
  const observationId = action.observationId.trim();
  const inputTokens = tokenCount(action.inputTokens);
  if (!observationId || inputTokens == null) return state;

  const index = state.usageObservations.findIndex(
    (observation) => observation.observationId === observationId,
  );
  const previous = index >= 0 ? state.usageObservations[index]! : null;
  const next: TurnTokenUsageObservation = {
    observationId,
    inputTokens: Math.max(previous?.inputTokens ?? 0, inputTokens),
    cacheReadTokens: maxObservedCount(
      previous?.cacheReadTokens ?? null,
      action.cacheReadTokens,
    ),
    cacheCreationTokens: maxObservedCount(
      previous?.cacheCreationTokens ?? null,
      action.cacheCreationTokens,
    ),
  };

  if (
    previous &&
    previous.inputTokens === next.inputTokens &&
    previous.cacheReadTokens === next.cacheReadTokens &&
    previous.cacheCreationTokens === next.cacheCreationTokens
  ) {
    return state;
  }

  const usageObservations = state.usageObservations.slice();
  if (index >= 0) usageObservations[index] = next;
  else usageObservations.push(next);
  return { ...state, usageObservations };
}

/** 归约一条当前回合观测；不同 turnId 或非法值均安全忽略。 */
export function reduceTurnLatency(
  state: TurnLatencyState,
  action: TurnLatencyAction,
): TurnLatencyState {
  if (action.turnId !== state.turnId) return state;
  switch (action.type) {
    case "send_acknowledged":
      return recordMilestone(state, "sendAcknowledgedAtMs", action.atMs);
    case "first_sse":
      return recordMilestone(state, "firstSseAtMs", action.atMs);
    case "first_visible_token":
      return recordMilestone(state, "firstVisibleTokenAtMs", action.atMs);
    case "completed":
      return recordMilestone(state, "completedAtMs", action.atMs);
    case "usage_observed":
      return recordUsage(state, action);
  }
}

/**
 * 复用当前 ACP usage_update 元数据生成本地观测动作。
 * replay 更新不代表当前运行时延，直接忽略。当前 Peri 契约要求每个
 * usage_update 携带稳定 llmStep；它是聚合主键，重复投递仍保持幂等。
 */
export function turnUsageActionFromAcp(
  turnId: string,
  update: UsageUpdateEvent,
): Extract<TurnLatencyAction, { type: "usage_observed" }> | null {
  const meta = update._meta ?? {};
  if (meta.periReplay === true) return null;
  const inputTokens = tokenCount(
    typeof meta.inputTokens === "number" ? meta.inputTokens : null,
  );
  if (inputTokens == null) return null;
  const llmStep = meta.llmStep;
  if (
    typeof llmStep !== "number" ||
    !Number.isInteger(llmStep) ||
    llmStep < 0
  ) {
    return null;
  }
  return {
    type: "usage_observed",
    turnId,
    observationId: `${turnId}:step:${llmStep}`,
    inputTokens,
    cacheReadTokens:
      typeof meta.cacheReadTokens === "number"
        ? meta.cacheReadTokens
        : null,
    cacheCreationTokens:
      typeof meta.cacheCreationTokens === "number"
        ? meta.cacheCreationTokens
        : null,
  };
}

function elapsedMs(state: TurnLatencyState, atMs: number | null): number | null {
  return atMs == null ? null : Math.max(0, atMs - state.startedAtMs);
}

function sumCounts(
  observations: readonly TurnTokenUsageObservation[],
  field: "inputTokens" | "cacheReadTokens" | "cacheCreationTokens",
): number | null {
  if (!observations.length) return null;
  let total = 0;
  for (const observation of observations) {
    const value = observation[field];
    if (value == null) return null;
    total += value;
  }
  return total;
}

/** 把绝对时间点与去重用量转换成可直接展示、持久化的相对指标。 */
export function summarizeTurnLatency(
  state: TurnLatencyState,
): TurnLatencySummary {
  const inputTokens = sumCounts(state.usageObservations, "inputTokens");
  const cacheReadTokens = sumCounts(
    state.usageObservations,
    "cacheReadTokens",
  );
  const cacheCreationTokens = sumCounts(
    state.usageObservations,
    "cacheCreationTokens",
  );
  return {
    turnId: state.turnId,
    sendAcknowledgementMs: elapsedMs(state, state.sendAcknowledgedAtMs),
    timeToFirstSseMs: elapsedMs(state, state.firstSseAtMs),
    timeToFirstVisibleTokenMs: elapsedMs(
      state,
      state.firstVisibleTokenAtMs,
    ),
    totalMs: elapsedMs(state, state.completedAtMs),
    inputTokens,
    cacheReadTokens,
    cacheCreationTokens,
  };
}

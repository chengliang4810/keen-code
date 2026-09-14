import { parseAcpTauriDelivery } from "./events";
import {
  commitLiveTurnToHistory,
  emptySession,
  reduceDeliveryEnvelope,
  reduceGoalSnapshot,
  type AcpSessionView,
} from "./store";

export interface HistoryPage {
  sessionId: string;
  nextCursor: string | null;
  hasMore: boolean;
  deliveries: unknown[];
}

export function parseHistoryPage(meta: Record<string, unknown> | undefined, sessionId: string): HistoryPage {
  const raw = meta?.["keencode/history"];
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error("ACP load 缺少历史分页信息");
  const page = raw as HistoryPage;
  if (page.sessionId !== sessionId || typeof page.hasMore !== "boolean" ||
    !Array.isArray(page.deliveries) ||
    (page.hasMore ? typeof page.nextCursor !== "string" || !page.nextCursor : page.nextCursor !== null)) {
    throw new Error("ACP load 历史分页信息无效");
  }
  return page;
}

/** 旧页在独立投影中归约；绝不回退实时水位、当前轮次及会话配置。 */
export function prependHistoryPage(view: AcpSessionView, page: HistoryPage): void {
  if (page.sessionId !== view.session_id) throw new Error("历史页 Session 不一致");
  const older = emptySession(view.session_id);
  older.replay.restoring = true;
  for (const raw of page.deliveries) {
    const delivery = parseAcpTauriDelivery(raw);
    if (!delivery || !("envelope" in delivery) || delivery.envelope.sessionId !== view.session_id ||
      reduceDeliveryEnvelope(older, delivery.envelope).status !== "applied") {
      throw new Error("历史页事件无效或不连续");
    }
  }
  commitLiveTurnToHistory(older);
  const existingIds = new Set(view.history.flatMap((message) => message.messageId ? [message.messageId] : []));
  const existingTurns = new Set(view.history.flatMap((message) => message.turnId ? [message.turnId] : []));
  if (view.active_root_turn_id) existingTurns.add(view.active_root_turn_id);
  view.history = [...older.history.filter((message) =>
    !(message.messageId && existingIds.has(message.messageId)) &&
    !(message.turnId && existingTurns.has(message.turnId))), ...view.history];
  view.terminal_turns = { ...older.terminal_turns, ...view.terminal_turns };
  for (const agent of view.subagents) {
    const previous = older.subagents.find((item) => item.agent_id === agent.agent_id);
    if (!previous) continue;
    const offset = previous.segments.length;
    agent.segments = [...previous.segments, ...agent.segments];
    const currentTurns = new Set(agent.turns?.map((turn) => turn.metrics.turnId));
    agent.turns = [
      ...(previous.turns ?? []).filter((turn) => !currentTurns.has(turn.metrics.turnId)),
      ...(agent.turns ?? []).map((turn) => {
        const prefix = previous.turns?.find((item) => item.metrics.turnId === turn.metrics.turnId);
        const usage = new Map(prefix?.metrics.usageObservations.map((item) => [item.observationId, item]));
        for (const item of turn.metrics.usageObservations) usage.set(item.observationId, item);
        const inheritedResult = prefix && turn.status !== "failed" && turn.status !== "interrupted"
          ? prefix.result
          : undefined;
        const inheritedError = prefix && turn.status === "failed"
          ? prefix.error
          : undefined;
        return {
          ...turn,
          segmentStart: prefix?.segmentStart ?? turn.segmentStart + offset,
          ...(turn.segmentEnd == null
            ? prefix?.segmentEnd == null ? {} : { segmentEnd: prefix.segmentEnd }
            : { segmentEnd: turn.segmentEnd + offset }),
          prompt: turn.prompt ?? prefix?.prompt,
          // 当前页代表较新的归约结果。只有当前 Turn 没有该字段时，
          // 才从更早页补齐，避免旧页结果覆盖续跑或最新终态。
          ...(Object.hasOwn(turn, "result")
            ? { result: turn.result }
            : inheritedResult !== undefined
              ? { result: inheritedResult }
              : {}),
          ...(Object.hasOwn(turn, "error")
            ? { error: turn.error }
            : inheritedError !== undefined
              ? { error: inheritedError }
              : {}),
          metrics: { ...turn.metrics, usageObservations: [...usage.values()] },
        };
      }),
    ];
  }
  const currentAgents = new Set(view.subagents.map((agent) => agent.agent_id));
  view.subagents = [...older.subagents.filter((agent) => !currentAgents.has(agent.agent_id)), ...view.subagents];
  // 历史前缀补齐此前未出现在最近窗口里的持久状态；revision 决定新旧。
  if (older.goal.revision > view.goal.revision) {
    reduceGoalSnapshot(view, older.goal.revision, older.goal.goal);
  }
  if (older.todos.revision > view.todos.revision) view.todos = older.todos;
  view.replay.hasMore = page.hasMore;
}

/** ACP Session 视图登记与完成后指标补写。 */

import {
  emptySession,
  type AcpSessionView,
  type AcpWorkspaceState,
} from "./store";
import type { TurnLatencySummary } from "../turnLatency";

/** 确保工作区中存在指定 Session 的视图；不存在时创建。 */
export function ensureAcpSession(
  workspace: AcpWorkspaceState,
  sessionId: string,
): AcpSessionView {
  let view = workspace.sessions[sessionId];
  if (!view) {
    view = emptySession(sessionId);
    workspace.sessions[sessionId] = view;
  }
  return view;
}

/** 用稳定 Turn 标识补写已经提交到历史的本轮指标。 */
export function replaceHistoryTurnMetrics(
  view: AcpSessionView,
  turnMetrics: TurnLatencySummary,
): boolean {
  for (let index = view.history.length - 1; index >= 0; index -= 1) {
    const message = view.history[index];
    if (message?.role === "assistant" && message.turnId === turnMetrics.turnId) {
      message.turnMetrics = turnMetrics;
      return true;
    }
  }
  return false;
}

export { commitLiveTurnToHistory } from "./store";

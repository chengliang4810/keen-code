/** ACP Session 视图登记、历史提交与重放归约。 */

import {
  emptySession,
  reduceSessionUpdate,
  type AcpSessionView,
  type AcpWorkspaceState,
} from "./store";
import type { SessionUpdate } from "./events";
import {
  compactMessageSegments,
  deriveFieldsFromSegments,
} from "../session";
import { parseAttachmentsFromContent } from "../attachments";
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

/** 把完成的实时 Turn 提交进历史并清空实时缓冲。 */
export function commitLiveTurnToHistory(
  view: AcpSessionView,
  options?: {
    /** peri 未回送用户内容时补入的本轮正文。 */
    userContent?: string;
    /** 本轮思考耗时，单位毫秒。 */
    thinkingDurationMs?: number;
    /** 本轮低延迟链路与缓存命中观测。 */
    turnMetrics?: TurnLatencySummary;
    /** 本轮实际使用的模型。 */
    model?: string;
  },
): void {
  const userContent = options?.userContent?.trim();
  const lastHistoryMessage = view.history.at(-1);
  const lastHistoryDisplayContent =
    lastHistoryMessage?.role === "user"
      ? parseAttachmentsFromContent(lastHistoryMessage.content).text.trim()
      : null;
  if (
    userContent &&
    !(
      lastHistoryMessage?.role === "user" &&
      (lastHistoryMessage.content === userContent ||
        lastHistoryDisplayContent === userContent)
    )
  ) {
    view.history.push({ role: "user", content: userContent });
  }
  const segments = compactMessageSegments(view.live_segments);
  const fields = deriveFieldsFromSegments(segments);
  const turnMetadata = view.live_turn_metadata;
  if (segments.length > 0 || turnMetadata) {
    view.history.push({
      role: "assistant",
      content: fields.content,
      ...(fields.thought ? { thought: fields.thought } : {}),
      segments,
      ...((turnMetadata?.durationMs ?? options?.thinkingDurationMs) != null
        ? {
            thinkingDurationMs:
              turnMetadata?.durationMs ?? options?.thinkingDurationMs,
          }
        : {}),
      ...(turnMetadata
        ? {
            turnStatus: turnMetadata.status,
            turnIncomplete: turnMetadata.incomplete,
            turnErrorKind: turnMetadata.errorKind,
          }
        : {}),
      ...(options?.turnMetrics != null
        ? { turnMetrics: options.turnMetrics }
        : {}),
      ...(turnMetadata?.model || options?.model
        ? { model: turnMetadata?.model ?? options?.model }
        : {}),
    });
  }
  view.live_segments = [];
  view.live_turn_metadata = null;
}

/**
 * 补写已经完成的回合指标。
 *
 * Tauri invoke 响应与事件通知走不同通道：极早失败时 agent-done 可能先于
 * session_send 的 accepted 响应到达。此时完成处理会先固化 Assistant 历史，
 * accepted 返回后再用 Host 的真实确认时间补齐同一 turn，而不能重新开启回合。
 */
export function replaceHistoryTurnMetrics(
  view: AcpSessionView,
  turnMetrics: TurnLatencySummary,
): boolean {
  for (let index = view.history.length - 1; index >= 0; index -= 1) {
    const message = view.history[index];
    if (
      message?.role === "assistant" &&
      message.turnMetrics?.turnId === turnMetrics.turnId
    ) {
      message.turnMetrics = turnMetrics;
      return true;
    }
  }
  return false;
}

/** 归约一条带 periReplay 标记的当前 session/update 结构。 */
export function reduceReplayedSessionUpdate(
  view: AcpSessionView,
  update: SessionUpdate,
  sourceAgentId?: string,
): void {
  switch (update.sessionUpdate) {
    case "user_message_chunk": {
      commitLiveTurnToHistory(view);
      reduceSessionUpdate(view, update, sourceAgentId);
      break;
    }
    case "agent_message_chunk":
    case "agent_thought_chunk":
    case "tool_call":
    case "tool_call_update": {
      reduceSessionUpdate(view, update, sourceAgentId);
      break;
    }
    case "plan":
    case "usage_update":
    case "available_commands_update":
    case "current_mode_update":
    case "config_option_update":
    case "session_info_update":
      break;
  }
}

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
  },
): void {
  const userContent = options?.userContent?.trim();
  const lastHistoryMessage = view.history.at(-1);
  if (
    userContent &&
    !(
      lastHistoryMessage?.role === "user" &&
      lastHistoryMessage.content === userContent
    )
  ) {
    view.history.push({ role: "user", content: userContent });
  }
  const segments = compactMessageSegments(view.live_segments);
  const fields = deriveFieldsFromSegments(segments);
  if (segments.length > 0) {
    view.history.push({
      role: "assistant",
      content: fields.content,
      ...(fields.thought ? { thought: fields.thought } : {}),
      segments,
      ...(options?.thinkingDurationMs != null
        ? { thinkingDurationMs: options.thinkingDurationMs }
        : {}),
    });
  }
  view.live_segments = [];
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

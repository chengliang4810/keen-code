import type { SessionListItem } from "./acp/api";
import type { Locale } from "@/i18n";
import type { SessionPreferences } from "./sessionPreferences";
import {
  mergeAttachments,
  mergeMessageAttachments,
  parseAttachmentsFromContent,
} from "./attachments";
import {
  applyTurnError,
  classifyAgentErrorCode,
  compactMessageSegments,
  deriveFieldsFromSegments,
  type ChatMessage,
  type MessageSegment,
  type SessionSnapshot,
  type SessionState,
} from "./session";
import type {
  AcpHistoryMessage,
  AcpSessionView,
} from "./acp/store";

/** 侧栏使用的项目展示项。 */
export interface ProjectView {
  /** 项目稳定标识。 */
  id: string;
  /** 项目显示名称。 */
  name: string;
  /** 项目规范化绝对路径。 */
  path: string;
  /** 项目目录当前是否可访问。 */
  pathOk: boolean;
}

/** 侧栏使用的 Session 展示项。 */
export interface SessionRowView {
  /** Session 稳定标识。 */
  id: string;
  /** Session 展示标题。 */
  title: string;
  /** Session 所属项目标识。 */
  projectId: string | null;
  /** Session 最近更新时间。 */
  updatedAt: string;
  /** Session 是否归档。 */
  archived: boolean;
  /** Session 是否置顶。 */
  pinned: boolean;
}

/** 侧栏所需的当前投影。 */
export interface SidebarProjection {
  /** KeenCode 当前登记的项目。 */
  projects: ProjectView[];
  /** peri ThreadStore 返回的 Session。 */
  sessions: SessionRowView[];
}

/** 将当前项目、Session 和本地展示偏好投影到侧栏。 */
export function projectSidebar(
  sessions: SessionListItem[],
  preferences: SessionPreferences,
  projects: ProjectView[],
): SidebarProjection {
  const projectByPath = new Map(
    projects.map((project) => [project.path, project.id] as const),
  );
  return {
    projects: projects.map((project) => ({ ...project })),
    sessions: sessions.map((session) => {
      const preference = preferences[session.id];
      return {
        id: session.id,
        title: preference?.title?.trim() || session.title?.trim() || "新对话",
        projectId: projectByPath.get(session.cwd) ?? null,
        updatedAt: session.updatedAt,
        archived: preference?.archived ?? false,
        pinned: preference?.pinned ?? false,
      };
    }),
  };
}

/** 将 ACP Session 状态收敛为工作台状态。 */
export function projectAcpSessionState(status: string): SessionState {
  switch (status) {
    case "attached":
    case "idle":
    case "ready":
      return "ready";
    case "connecting":
      return "connecting";
    case "streaming":
      return "streaming";
    case "disconnected":
      return "disconnected";
    default:
      throw new Error(`未知 ACP Session 状态：${status}`);
  }
}

/** 将 ACP Session 视图投影为工作台外壳状态。 */
export function projectAcpSnapshot(view: AcpSessionView): SessionSnapshot {
  return {
    sessionId: view.session_id,
    state: projectAcpSessionState(view.status),
    lastError: view.last_error
      ? {
          code: classifyAgentErrorCode(
            view.last_error.code,
            view.last_error.message,
          ),
          message: `${view.last_error.code}: ${view.last_error.message}`,
        }
      : null,
    streamingMessageId: null,
    backend: "peri_acp",
    projectPath: view.project_path,
    title: view.title?.trim() || "新对话",
  };
}

/** 将 ACP 历史消息投影为工作台消息。 */
export function projectAcpHistory(
  sessionId: string,
  source: AcpHistoryMessage[],
): ChatMessage[] {
  const projected: ChatMessage[] = source.map((message, index) => {
    const role =
      message.role === "assistant" || message.role === "tool"
        ? message.role
        : "user";
    const parsed = parseAttachmentsFromContent(message.content);
    const attachments = mergeMessageAttachments(
      mergeAttachments(parsed.attachments, message.attachments ?? []),
      message.content,
    );
    const segmentFields = message.segments
      ? deriveFieldsFromSegments(message.segments)
      : null;
    return {
      id: `${sessionId}:history:${index}`,
      role,
      content: role === "user" ? parsed.text : message.content,
      thought: segmentFields?.thought ?? message.thought,
      thoughtPhases:
        segmentFields?.thoughtPhases ??
        (message.thought ? [message.thought] : undefined),
      segments: message.segments?.map((segment) => ({ ...segment })),
      thinkingDurationMs: message.thinkingDurationMs,
      turnStatus: message.turnStatus,
      turnIncomplete: message.turnIncomplete,
      turnErrorKind: message.turnErrorKind,
      turnMetrics: message.turnMetrics,
      model: message.model,
      marker: message.marker,
      compactMeta: message.compactMeta,
      systemNotificationLevel: message.systemNotificationLevel,
      attachments,
      streaming: false,
    };
  });
  for (let index = 0; index < projected.length; index += 1) {
    const model = projected[index]?.role === "assistant" ? projected[index]?.model : undefined;
    if (!model) continue;
    for (let userIndex = index - 1; userIndex >= 0; userIndex -= 1) {
      const user = projected[userIndex];
      if (user?.role !== "user") continue;
      user.model ??= model;
      break;
    }
  }
  return projected;
}

/** 将 ACP 当前 Turn 投影为一条可替换的 Assistant 消息。 */
export function projectAcpLiveMessage(
  view: AcpSessionView,
): ChatMessage | null {
  const segments: MessageSegment[] = compactMessageSegments(
    view.live_segments,
  );
  const turnMetadata = view.live_turn_metadata;
  if (segments.length === 0 && !turnMetadata) return null;
  const fields = deriveFieldsFromSegments(segments);
  return {
    id: `${view.session_id}:live`,
    role: "assistant",
    content: fields.content,
    thought: fields.thought,
    thoughtPhases: fields.thoughtPhases,
    segments,
    thinkingDurationMs: turnMetadata?.durationMs,
    turnStatus: turnMetadata?.status,
    turnIncomplete: turnMetadata?.incomplete,
    turnErrorKind: turnMetadata?.errorKind,
    streaming: view.status === "streaming",
  };
}

/** 用 ACP 当前 Turn 替换消息列表中的临时 Assistant 气泡。 */
export function mergeAcpLiveMessage(
  previous: ChatMessage[],
  view: AcpSessionView,
): ChatMessage[] {
  const liveId = `${view.session_id}:live`;
  const base = previous.filter((message) => message.id !== liveId);
  const live = projectAcpLiveMessage(view);
  if (live) {
    // live 内容出现后用真实消息替换发送时的乐观 Assistant 气泡。
    return [...base.filter((message) => !message.id.startsWith("a-pending-")), live];
  }
  // 尚无 live 内容（首 token 未到）时保留乐观 Assistant 气泡：
  // 发送后立即显示回合计时，避免中途事件把计时器抹掉。
  return base;
}

/** 将当前回合错误投影为稳定的 Assistant 错误气泡。 */
export function mergeAcpTurnError(
  previous: ChatMessage[],
  view: AcpSessionView,
  locale: Locale = "zh",
): ChatMessage[] {
  if (!view.last_error) return previous;
  return applyTurnError(
    previous,
    {
      messageId: `${view.session_id}:turn-error`,
      code: view.last_error.code,
      message: view.last_error.message,
    },
    locale,
  );
}

/**
 * 把持久历史、当前 ACP Turn 与本地乐观消息合成一份完整会话投影。
 *
 * 新建 Session 的 connect/replay 可能发生在 `sessionSend` 之前；这段窗口内
 * ACP 尚无 live segment，必须显式保留本地 pending Assistant，才能持续展示
 * 已有的处理耗时反馈。上一轮错误不属于历史，下一轮开始后不会被带入。
 */
export function projectAcpConversation(
  previous: ChatMessage[],
  view: AcpSessionView,
  locale: Locale = "zh",
  keepPendingAssistant = false,
): ChatMessage[] {
  const history = projectAcpHistory(view.session_id, view.history);
  const optimistic = previous.filter((message) => {
    if (
      message.role === "user" &&
      message.id.startsWith("u-") &&
      !history.some(
        (stored) =>
          stored.role === "user" && stored.content === message.content,
      )
    ) {
      return true;
    }
    return (
      keepPendingAssistant &&
      message.role === "assistant" &&
      message.id.startsWith("a-pending-") &&
      message.streaming === true
    );
  });

  return mergeAcpTurnError(
    mergeAcpLiveMessage([...history, ...optimistic], view),
    view,
    locale,
  );
}

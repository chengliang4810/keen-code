import type { SessionListItem } from "./acp/api";
import type { Locale } from "@/i18n";
import type { SessionPreferences } from "./sessionPreferences";
import { parseStoredContent, serializeForAgent } from "./draftDoc";
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
  isModelOnlySyntheticInput,
} from "./session";
import type {
  AcpHistoryMessage,
  AcpSessionView,
  AcpSubagentInfo,
} from "./acp/store";
import { summarizeTurnLatency } from "./turnLatency";

/** 根 Turn 的 Assistant 在实时流和历史中共用同一 React 身份。 */
function assistantTurnMessageId(
  sessionId: string,
  turnId: string | null | undefined,
): string {
  return turnId ? `${sessionId}:turn:${turnId}` : `${sessionId}:live`;
}

/** 子 Agent 续跑按 Turn 展示，正文和用量共享同一边界。 */
export function projectSubagentConversation(agent: AcpSubagentInfo): ChatMessage[] {
  const messages: ChatMessage[] = [];
  const turns = agent.turns ?? [];
  if (!turns.length && agent.prompt) {
    messages.push({ id: `subagent-${agent.agent_id}-prompt`, role: "user", content: agent.prompt });
  }
  for (const [index, turn] of turns.entries()) {
    const id = `subagent-${agent.agent_id}-${turn.metrics.turnId}`;
    const prompt = turn.prompt ?? (index === 0 ? agent.prompt : "");
    if (prompt) messages.push({ id: `${id}-prompt`, role: "user", content: prompt });
    const segments = agent.segments.slice(turn.segmentStart, turn.segmentEnd);
    const fields = deriveFieldsFromSegments(segments);
    const turnMetrics = summarizeTurnLatency(turn.metrics);
    const result = turn.error ??
      turn.result ??
      (index === turns.length - 1 && turn.status !== "running"
        ? agent.result
        : undefined) ??
      fields.content;
    messages.push({
      id, role: "assistant", segments, turnMetrics,
      // 终态结果是 Runtime/Reducer 固化的字段；失败优先展示稳定错误，
      // 只有旧数据或中断 Turn 没有结果时才从完整时间线回退正文。
      content: result || "",
      streaming: turn.status === "running",
      ...(turnMetrics.totalMs != null ? { thinkingDurationMs: turnMetrics.totalMs } : {}),
    });
  }
  return messages;
}

/** 侧栏使用的项目展示项。 */
export interface ProjectView {
  /** 项目稳定标识。 */
  id: string;
  /** 项目显示名称。 */
  name: string;
  /** 项目规范化绝对路径。 */
  path: string;
  /** 项目目录当前是否可访问。 */
  pathOk: boolean | null;
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
  /** Session 最近一条用户消息时间；从未发送消息时为空。 */
  lastUserMessageAt: string | null;
  /** Session 是否归档。 */
  archived: boolean;
  /** Session 是否置顶。 */
  pinned: boolean;
}

/** 侧栏所需的当前投影。 */
export interface SidebarProjection {
  /** KeenCode 当前登记的项目。 */
  projects: ProjectView[];
  /** Agent Runtime 返回的 Session。 */
  sessions: SessionRowView[];
}

/** 规范化 Session 与项目关联使用的路径身份，并兼容 Windows 扩展路径前缀。 */
function normalizeSessionProjectPath(path: string): string {
  let normalized = path.trim().replace(/\\/g, "/");
  const lower = normalized.toLowerCase();
  if (lower.startsWith("//?/unc/")) {
    normalized = `//${normalized.slice(8)}`;
  } else if (lower.startsWith("//?/")) {
    normalized = normalized.slice(4);
  }
  const windowsPath =
    /^[A-Za-z]:\//.test(normalized) || normalized.startsWith("//");
  if (windowsPath) normalized = normalized.toLowerCase();
  if (/^[a-zA-Z]:\/$/.test(normalized) || normalized === "/") {
    return normalized;
  }
  return normalized.replace(/\/+$/, "");
}

/** 将当前项目、Session 和本地展示偏好投影到侧栏。 */
export function projectSidebar(
  sessions: SessionListItem[],
  preferences: SessionPreferences,
  projects: ProjectView[],
): SidebarProjection {
  const projectByPath = new Map(
    projects.map(
      (project) =>
        [normalizeSessionProjectPath(project.path), project.id] as const,
    ),
  );
  return {
    projects: projects.map((project) => ({ ...project })),
    sessions: sessions.map((session) => {
      const preference = preferences[session.id];
      return {
        id: session.id,
        title: preference?.title?.trim() || session.title?.trim() || "新对话",
        projectId:
          projectByPath.get(normalizeSessionProjectPath(session.cwd)) ?? null,
        updatedAt: session.updatedAt,
        lastUserMessageAt: session.lastUserMessageAt,
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
    state: view.replay.restoring ? "connecting" : projectAcpSessionState(view.status),
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
    backend: "acp",
    projectPath: view.project_path,
    title: view.title?.trim() || "新对话",
  };
}

/**
 * 历史投影缓存：键为历史数组身份。
 *
 * store 对 history 的既有元素存在原地修改（分片追加、指标/模型补写），
 * 这些写入用 `AcpSessionView.history_revision` 递增标记；因此命中条件为
 * 「数组身份 + 长度 + 修订号 + Session」。流式回合的分片写入
 * live_segments 而非 history，命中后每帧省去全量重投影（实测 13MB
 * 历史约 40ms 与 4MB 垃圾/帧）。数组被替换时 WeakMap 自动释放。
 */
const historyProjectionCache = new WeakMap<
  AcpHistoryMessage[],
  { sessionId: string; length: number; revision: number; result: ChatMessage[] }
>();

type LiveTextProjectionCache = {
  /** 已归约的正文，避免每个绘制批次重新遍历并 join 全量文本。 */
  content: string;
  /** 与 deriveFieldsFromSegments 相同语义的思考阶段。 */
  thoughtPhases: string[];
  /** 已扫描到的段数量；流式路径只会在尾部追加或原地增长。 */
  processedSegmentCount: number;
  /** 最近一次扫描到的文本段，用于吸收 appendText 的原地增长。 */
  lastTextSegment: Extract<MessageSegment, { kind: "content" | "thought" }> | null;
  lastTextSegmentIndex: number;
  lastTextLength: number;
};

/**
 * 实时段由 ACP reducer 维护为 append-only 文本段：相邻同类 delta 在尾段
 * 原地追加，工具/压缩段只改变结构化字段。按数组身份缓存正文与思考字段，
 * 每帧只处理新增段或尾段新增字符；数组被截断/重排时自动回退一次全文扫描。
 */
const liveTextProjectionCache = new WeakMap<
  MessageSegment[],
  LiveTextProjectionCache
>();

function rebuildLiveTextProjection(
  segments: MessageSegment[],
): LiveTextProjectionCache {
  const contentParts: string[] = [];
  const thoughtPhases: string[] = [];
  let lastTextSegment: LiveTextProjectionCache["lastTextSegment"] = null;
  let lastTextSegmentIndex = -1;
  for (const [index, segment] of segments.entries()) {
    if (segment.kind === "content") {
      contentParts.push(segment.text);
      lastTextSegment = segment;
      lastTextSegmentIndex = index;
    } else if (segment.kind === "thought") {
      thoughtPhases.push(segment.text);
      lastTextSegment = segment;
      lastTextSegmentIndex = index;
    }
  }
  const cache: LiveTextProjectionCache = {
    content: contentParts.join(""),
    thoughtPhases,
    processedSegmentCount: segments.length,
    lastTextSegment,
    lastTextSegmentIndex,
    lastTextLength: lastTextSegment?.text.length ?? 0,
  };
  liveTextProjectionCache.set(segments, cache);
  return cache;
}

/**
 * 从 Web Host 的 Session 列表派生项目树。
 *
 * Desktop 的项目登记属于 Tauri 本地状态，Web 只允许通过 ACP 读取 Session
 * 的 `cwd`，因此这里使用规范化路径作为稳定身份，避免在前端复制一份项目
 * 持久化事实。该投影只用于 Web 侧栏，项目写操作仍按宿主 capability 禁用。
 */
export function projectsFromSessions(sessions: SessionListItem[]): ProjectView[] {
  const seen = new Set<string>();
  const projects: ProjectView[] = [];
  for (const session of sessions) {
    const path = session.cwd.trim();
    if (!path) continue;
    const normalized = normalizeSessionProjectPath(path);
    if (!normalized || seen.has(normalized)) continue;
    seen.add(normalized);
    const displayPath = path.replace(/\\/g, "/");
    const withoutTrailing = displayPath.replace(/\/+$/, "") || displayPath;
    const name =
      withoutTrailing.split("/").at(-1)?.trim() ||
      (withoutTrailing.match(/^[a-z]:$/i)?.[0] ?? withoutTrailing);
    projects.push({
      id: `web-project:${encodeURIComponent(normalized)}`,
      name,
      path: session.cwd.trim(),
      pathOk: true,
    });
  }
  return projects;
}

function liveTextFields(segments: MessageSegment[]): {
  content: string;
  thought: string | undefined;
  thoughtPhases: string[] | undefined;
} {
  let cache = liveTextProjectionCache.get(segments);
  if (!cache) cache = rebuildLiveTextProjection(segments);

  const previousLast = cache.lastTextSegment;
  if (
    previousLast &&
    segments[cache.lastTextSegmentIndex] !== previousLast
  ) {
    cache = rebuildLiveTextProjection(segments);
  } else if (cache.processedSegmentCount > segments.length) {
    cache = rebuildLiveTextProjection(segments);
  } else {
    // The reducer only mutates the final text segment by appending. A shrink
    // means an external/manual mutation, so discard the incremental state.
    const currentLength = previousLast?.text.length ?? 0;
    if (currentLength < cache.lastTextLength) {
      cache = rebuildLiveTextProjection(segments);
    } else if (previousLast && currentLength > cache.lastTextLength) {
      const delta = previousLast.text.slice(cache.lastTextLength);
      if (previousLast.kind === "content") {
        cache.content += delta;
      } else {
        const phase = cache.thoughtPhases.at(-1);
        if (phase === undefined) cache.thoughtPhases.push(previousLast.text);
        else cache.thoughtPhases[cache.thoughtPhases.length - 1] = phase + delta;
      }
      cache.lastTextLength = currentLength;
    }

    for (let index = cache.processedSegmentCount; index < segments.length; index += 1) {
      const segment = segments[index];
      if (segment?.kind === "content") {
        cache.content += segment.text;
        cache.lastTextSegment = segment;
        cache.lastTextSegmentIndex = index;
        cache.lastTextLength = segment.text.length;
      } else if (segment?.kind === "thought") {
        cache.thoughtPhases.push(segment.text);
        cache.lastTextSegment = segment;
        cache.lastTextSegmentIndex = index;
        cache.lastTextLength = segment.text.length;
      }
    }
    cache.processedSegmentCount = segments.length;
  }

  const thoughts = cache.thoughtPhases.filter((text) => text.trim());
  return {
    content: cache.content,
    thought: thoughts.length ? thoughts.join("\n\n⟪phase⟫\n\n") : undefined,
    thoughtPhases: thoughts.length ? thoughts : undefined,
  };
}

/** 将 ACP 历史消息投影为工作台消息。 */
export function projectAcpHistory(
  sessionId: string,
  source: AcpHistoryMessage[],
  revision = 0,
): ChatMessage[] {
  const cached = historyProjectionCache.get(source);
  if (
    cached &&
    cached.sessionId === sessionId &&
    cached.length === source.length &&
    cached.revision === revision
  ) {
    return cached.result;
  }
  const projected: ChatMessage[] = source
    .filter(
      (message) =>
        !(
          message.role === "user" &&
          isModelOnlySyntheticInput(message.content)
        ),
    )
    .map((message, index) => {
      const role =
      message.role === "assistant" || message.role === "tool"
        ? message.role
        : "user";
    const parsed = parseAttachmentsFromContent(message.content);
    const storedAttachments = mergeAttachments(
      parsed.attachments,
      message.attachments ?? [],
    );
    const attachments =
      role === "user"
        ? storedAttachments.length
          ? storedAttachments
          : undefined
        : mergeMessageAttachments(storedAttachments, message.content);
    const segmentFields = message.segments
      ? deriveFieldsFromSegments(message.segments)
      : null;
    return {
      // Runtime 标识优先；根 Assistant 用 Turn 标识跨越实时/历史，仅旧数据按位置回退。
      id: message.messageId ??
        (role === "assistant" && message.turnId
          ? assistantTurnMessageId(sessionId, message.turnId)
          : `${sessionId}:history:${index}`),
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
  historyProjectionCache.set(source, {
    sessionId,
    length: source.length,
    revision,
    result: projected,
  });
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
  const fields =
    segments.length === view.live_segments.length
      ? liveTextFields(view.live_segments)
      : deriveFieldsFromSegments(segments);
  return {
    id: assistantTurnMessageId(
      view.session_id,
      view.active_root_turn_id,
    ),
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
  const liveId = assistantTurnMessageId(
    view.session_id,
    view.active_root_turn_id,
  );
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
 * 取得用于乐观消息去重的最后一条已持久化用户消息。
 *
 * 活动 Turn 内只认当前 Turn 的持久消息；Turn 结束后 active_root_turn_id
 * 已清空，此时按历史最后一条去重，否则失败 Turn 的乐观气泡会与权威消息
 * 重复显示，且其本地 id 无法作为编辑重发的回退锚点。
 */
function latestPersistedCurrentTurnUser(
  view: AcpSessionView,
): AcpHistoryMessage | undefined {
  const turnId = view.active_root_turn_id;
  for (let index = view.history.length - 1; index >= 0; index -= 1) {
    const stored = view.history[index];
    if (stored?.role !== "user") continue;
    if (turnId && stored.turnId !== turnId) continue;
    return stored;
  }
  return undefined;
}

/** 判断当前根 Turn 是否已经持久化了对应的用户消息。 */
function hasPersistedCurrentTurnUser(
  view: AcpSessionView,
  optimistic: ChatMessage,
): boolean {
  const stored = latestPersistedCurrentTurnUser(view);
  return stored !== undefined &&
    parseAttachmentsFromContent(stored.content).text ===
      serializeForAgent(parseStoredContent(optimistic.content));
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
  const history = projectAcpHistory(
    view.session_id,
    view.history,
    view.history_revision,
  );
  const optimistic = previous.filter((message) => {
    if (
      message.role === "user" &&
      message.id.startsWith("u-") &&
      !hasPersistedCurrentTurnUser(view, message)
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

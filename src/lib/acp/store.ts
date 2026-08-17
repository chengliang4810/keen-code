/** ACP 事件归约器：把 acp://* 通知归约为当前 UI 会话视图。 */

import type {
  AcpEvent,
  GoalRecordDto,
  PendingToolItem,
  SessionUpdate,
} from "./events";
import type { Attachment } from "../attachments";
import type {
  ContextCompactMeta,
  MessageSegment,
  MessageToolSegment,
} from "../session";
import type { TurnLatencySummary } from "../turnLatency";
import type {
  AcpRetryProjection,
  AcpSystemNotificationLevel,
} from "./types";

export interface AcpHistoryMessage {
  /** 消息角色。 */
  role: string;
  /** 原始消息正文。 */
  content: string;
  /** 随历史消息保留或从正文引用恢复的本地附件。 */
  attachments?: Attachment[];
  /** Assistant 思考正文，完成后保留用于折叠查看。 */
  thought?: string;
  /** 从收到用户消息到本轮完成的耗时。 */
  thinkingDurationMs?: number;
  /** 本轮 Host、Provider、可见首 Token、完成与缓存命中观测。 */
  turnMetrics?: TurnLatencySummary;
  /** 完成本轮时固化的思考、工具与正文顺序。 */
  segments?: MessageSegment[];
  /** 时间线系统标记。 */
  marker?: string;
  /** 上下文压缩详情。 */
  compactMeta?: ContextCompactMeta;
  /** 系统通知的归一化等级。 */
  systemNotificationLevel?: AcpSystemNotificationLevel;
}

export interface AcpGoalProjection {
  revision: number;
  goal: GoalRecordDto | null;
}

export interface AcpTodoProjection {
  revision: number;
  items: Array<{ content: string; status: string }>;
}

export interface AcpSubagentInfo {
  agent_id: string;
  agent_name: string;
  status: "running" | "done" | "failed";
  /** 子 Agent 是否在后台运行。 */
  is_background: boolean;
  /** 启动时间（Unix 毫秒）。 */
  started_at: number;
  /** 结束时间（Unix 毫秒）；运行中为 null。 */
  stopped_at: number | null;
  /** 最终结果文本；未结束时为 null。 */
  result: string | null;
  /** 子 Agent 时间线分片。 */
  segments: MessageSegment[];
}

export interface AcpReplayProjection {
  cursor: { epoch: string; sequence: number } | null;
  pending_tools: PendingToolItem[];
  restoring: boolean;
}

export interface AcpToolSearchProjection {
  query: string;
  tools: unknown[];
  total: number;
  truncated: boolean;
  catalog_revision: number;
}

export interface AcpSessionView {
  session_id: string;
  /** Session 当前绑定的项目绝对路径。 */
  project_path: string | null;
  /** "attached" | "streaming" | "idle" 等展示状态。 */
  status: string;
  /** 当前 Turn 按 ACP 到达顺序维护的唯一时间线。 */
  live_segments: MessageSegment[];
  history: AcpHistoryMessage[];
  input_tokens: number;
  output_tokens: number;
  cache_read_input_tokens?: number | null;
  last_error: { code: string; message: string } | null;
  goal: AcpGoalProjection;
  todos: AcpTodoProjection;
  subagents: AcpSubagentInfo[];
  replay: AcpReplayProjection;
  tool_search: AcpToolSearchProjection | null;
  reasoning_effort?: string | null;
  compacting: boolean;
  /** 当前供应商模型重试状态。 */
  retry: AcpRetryProjection | null;
  title?: string | null;
  /** 当前轮次收到用户消息的时间戳。 */
  turn_started_at: number | null;
}

export interface AcpWorkspaceState {
  sessions: Record<string, AcpSessionView>;
}

export function createAcpWorkspaceState(): AcpWorkspaceState {
  return { sessions: {} };
}

export function emptySession(session_id: string): AcpSessionView {
  return {
    session_id,
    project_path: null,
    status: "attached",
    live_segments: [],
    history: [],
    input_tokens: 0,
    output_tokens: 0,
    cache_read_input_tokens: null,
    last_error: null,
    goal: { revision: 0, goal: null },
    todos: { revision: 0, items: [] },
    subagents: [],
    replay: { cursor: null, pending_tools: [], restoring: false },
    tool_search: null,
    reasoning_effort: null,
    compacting: false,
    retry: null,
    title: null,
    turn_started_at: null,
  };
}

/** 从 ACP SessionUpdate 的当前内容结构读取文本。 */
function textOf(value: unknown): string {
  if (!value || typeof value !== "object") return "";
  const update = value as {
    content?: { type?: string; text?: string };
  };
  const content = update.content;
  if (!content) return "";
  if (content.type === "text" && typeof content.text === "string") {
    return content.text;
  }
  return "";
}

/** 将任意 ACP 工具值转换为时间线文本。 */
function stringifyToolValue(value: unknown): string | undefined {
  if (value === undefined || value === null) return undefined;
  return typeof value === "string" ? value : JSON.stringify(value);
}

/** 按到达顺序追加正文或思考，相邻同类分片原地合并。 */
function appendText(
  segments: MessageSegment[],
  kind: "content" | "thought",
  text: string,
): void {
  if (!text) return;
  const last = segments.at(-1);
  if (last?.kind === kind) {
    last.text += text;
    return;
  }
  segments.push({ kind, text });
}

/** 查找指定时间线中的 ACP 工具段。 */
function findToolIn(
  segments: MessageSegment[],
  toolCallId: string,
): MessageToolSegment | undefined {
  return segments.find(
    (segment): segment is MessageToolSegment =>
      segment.kind === "tool" && segment.toolCallId === toolCallId,
  );
}

/**
 * 定位更新应写入的时间线：存在 sourceAgentId 时返回对应子 Agent 的
 * segments（找不到该 Agent 则返回 null 表示忽略），否则返回实时缓冲。
 */
function targetSegments(
  view: AcpSessionView,
  sourceAgentId: string | undefined,
): MessageSegment[] | null {
  if (sourceAgentId === undefined) return view.live_segments;
  const agent = view.subagents.find((a) => a.agent_id === sourceAgentId);
  return agent ? agent.segments : null;
}

/**
 * 将 Peri 3.6.5 的事件来源身份归一化为 KeenCode 的子 Agent 路由提示。
 *
 * Peri v2 会给主 Agent 与子 Agent 的渲染事件都附加 sourceAgentId；只有已经由
 * subagent_started 登记的身份才属于子 Agent，其余身份必须保留在主时间线。
 */
export function resolveSessionUpdateSourceAgentId(
  view: AcpSessionView,
  sourceAgentId: string | null | undefined,
): string | undefined {
  if (!sourceAgentId) return undefined;
  return view.subagents.some((agent) => agent.agent_id === sourceAgentId)
    ? sourceAgentId
    : undefined;
}

/** 归约一条 session/update。返回是否发生了状态变化。 */
export function reduceSessionUpdate(
  view: AcpSessionView,
  update: SessionUpdate,
  sourceAgentId?: string,
): void {
  switch (update.sessionUpdate) {
    case "user_message_chunk": {
      // 新一轮已经开始，上一轮错误已由消息投影固化为错误气泡。
      view.last_error = null;
      if (!sourceAgentId) view.retry = null;
      const text = textOf(update);
      if (text) {
        view.history.push({ role: "user", content: text });
      }
      break;
    }
    case "agent_message_chunk": {
      if (!sourceAgentId) view.retry = null;
      const segments = targetSegments(view, sourceAgentId);
      if (segments) appendText(segments, "content", textOf(update));
      break;
    }
    case "agent_thought_chunk": {
      if (!sourceAgentId) view.retry = null;
      const segments = targetSegments(view, sourceAgentId);
      if (segments) appendText(segments, "thought", textOf(update));
      break;
    }
    case "tool_call": {
      if (!sourceAgentId) view.retry = null;
      const input = stringifyToolValue(update.rawInput);
      const tool: MessageToolSegment = {
        kind: "tool",
        toolCallId: update.toolCallId,
        title: update.title,
        toolKind: update.title,
        status: update.status ?? "pending",
        input,
        detail: input,
        streaming:
          update.status == null ||
          update.status === "pending" ||
          update.status === "in_progress",
      };
      const segments = targetSegments(view, sourceAgentId);
      if (!segments) break;
      const existing = findToolIn(segments, update.toolCallId);
      if (existing) Object.assign(existing, tool);
      else segments.push(tool);
      break;
    }
    case "tool_call_update": {
      if (!sourceAgentId) view.retry = null;
      const segments = targetSegments(view, sourceAgentId);
      if (!segments) break;
      const tool = findToolIn(segments, update.toolCallId);
      if (tool) {
        if (update.status) tool.status = update.status;
        if (update.title) {
          tool.title = update.title;
          tool.toolKind = update.title;
        }
        if (update.rawOutput !== undefined) {
          tool.output = stringifyToolValue(update.rawOutput);
          tool.detail = tool.output ?? tool.input;
        }
        tool.streaming =
          tool.status === "pending" || tool.status === "in_progress";
        tool.isError = tool.status === "failed";
      }
      break;
    }
    case "plan": {
      if (!sourceAgentId) view.retry = null;
      view.todos.revision += 1;
      view.todos.items = update.entries.map((e) => ({
        content: e.content,
        status: e.status,
      }));
      break;
    }
    case "usage_update": {
      const meta = update._meta ?? {};
      view.input_tokens = Number(meta.inputTokens ?? 0);
      view.output_tokens = Number(meta.outputTokens ?? 0);
      view.cache_read_input_tokens = Number(meta.cacheReadTokens ?? 0);
      break;
    }
    case "session_info_update": {
      if (typeof update.title === "string") view.title = update.title;
      break;
    }
    case "available_commands_update":
    case "current_mode_update":
    case "config_option_update":
      break;
  }
}

/** 归约一条 peri/agent_event。 */
export function reduceAgentEvent(
  view: AcpSessionView,
  event: AcpEvent,
): void {
  switch (event.type) {
    case "turn_suspended": {
      view.status = "ready";
      view.retry = null;
      break;
    }
    case "subagent_started": {
      const v = event.value;
      view.subagents = view.subagents.filter((s) => s.agent_id !== v.instance_id);
      view.subagents.push({
        agent_id: v.instance_id,
        agent_name: v.agent_name,
        status: "running",
        is_background: v.is_background,
        started_at: Date.now(),
        stopped_at: null,
        result: null,
        segments: [],
      });
      break;
    }
    case "subagent_stopped": {
      const v = event.value;
      view.subagents = view.subagents
        .filter((s) => s.agent_id !== v.instance_id)
        .concat(
          view.subagents
            .filter((s) => s.agent_id === v.instance_id)
            .map((s) => ({
              ...s,
              agent_name: v.agent_name,
              status: v.is_error ? ("failed" as const) : ("done" as const),
              stopped_at: Date.now(),
              result: v.result,
            })),
        );
      break;
    }
    case "compact_started": {
      view.compacting = true;
      break;
    }
    case "compact_completed": {
      view.compacting = false;
      view.history.push({
        role: "tool",
        content: "context_compact",
        marker: "context_compact",
        compactMeta: {
          trigger: event.value.trigger,
          summaryPreview: event.value.summary || undefined,
        },
      });
      break;
    }
    case "compact_error": {
      view.compacting = false;
      break;
    }
    case "goal_changed": {
      const v = event.value;
      view.goal = { revision: v.revision, goal: v.goal };
      break;
    }
    case "agent_execution_failed": {
      const v = event.value;
      view.retry = null;
      view.last_error = { code: "agent_execution_failed", message: v.message };
      break;
    }
    case "system_notification": {
      const v = event.value;
      view.history.push({
        role: "tool",
        content: v.text,
        marker: "system_notification",
        systemNotificationLevel: v.level,
      });
      break;
    }
    case "llm_retrying": {
      const v = event.value;
      view.retry = {
        attempt: v.attempt,
        maxAttempts: v.max_attempts,
        delayMs: v.delay_ms,
        reason: v.error,
      };
      break;
    }
    default:
      // context_warning 等当前事件暂不进入 UI 投影。
      break;
  }
}

/** 归约 session/recovery 通知。 */
export function reduceRecovery(
  view: AcpSessionView,
  params: {
    status: string;
    cursor?: { epoch: string; sequence: number } | null;
    pending_tools: PendingToolItem[];
    reason?: string | null;
  },
): void {
  view.replay = {
    cursor: params.cursor ?? null,
    pending_tools: params.pending_tools ?? [],
    restoring: params.status === "restoring",
  };
  if (params.pending_tools && params.pending_tools.length > 0) {
    view.last_error = {
      code: "pending_tools",
      message: `${params.pending_tools.length} 个工具调用在中断时未完成，结果状态未知`,
    };
  }
}

/** 归约 goal_get 查询结果（全量替换）。 */
export function reduceGoalSnapshot(
  view: AcpSessionView,
  revision: number,
  goals: GoalRecordDto[],
): void {
  view.goal = { revision, goal: goals[0] ?? null };
}

/** 归约 session/replay 响应（游标推进）。 */
export function reduceReplayResult(
  view: AcpSessionView,
  result: {
    next: { epoch: string; sequence: number };
    replayed_events: number;
    truncated: boolean;
  },
): void {
  view.replay.cursor = result.next;
}

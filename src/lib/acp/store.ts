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
  MessageToolSegment,
  MessageSegment,
} from "../session";
import type { TurnLatencySummary } from "../turnLatency";
import type {
  AcpArtifactReference,
  AcpFileOperation,
  AcpRetryProjection,
  AcpStructuredToolResult,
  AcpSystemNotificationLevel,
  AcpToolResultItem,
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
  /** 持久化 Turn 状态；不与面向用户的错误正文混用。 */
  turnStatus?: "completed" | "failed" | "cancelled";
  /** Turn 是否包含未完成的模型输出。 */
  turnIncomplete?: boolean;
  /** 归一化错误类别；原始错误详情只存在于诊断记录。 */
  turnErrorKind?: string;
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
  /** 主 Agent 委派给该子 Agent 的原始任务。 */
  prompt?: string;
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
  /** replay 或实时完成时附着在当前 Assistant Turn 上的持久化元数据。 */
  live_turn_metadata: {
    status: "completed" | "failed" | "cancelled";
    durationMs?: number;
    incomplete: boolean;
    errorKind?: string;
  } | null;
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
    live_turn_metadata: null,
  };
}

/**
 * 本地发送已经建立新回合时，立即清理仅属于上一轮的瞬时状态。
 *
 * Peri 会持久化用户消息，但实时链路不保证回送 `user_message_chunk`；因此
 * 上一轮错误不能只依赖该事件清理，否则它会一直污染下一轮的运行投影。
 */
export function beginLocalSessionTurn(
  view: AcpSessionView,
  startedAt: number,
): void {
  view.status = "streaming";
  view.last_error = null;
  view.retry = null;
  view.turn_started_at = startedAt;
  view.live_turn_metadata = null;
}

function captureTurnMetadata(view: AcpSessionView, update: SessionUpdate): void {
  const meta = (update as { _meta?: Record<string, unknown> })._meta;
  const status = meta?.turnStatus;
  if (status !== "completed" && status !== "failed" && status !== "cancelled") {
    return;
  }
  const duration = Number(meta?.turnDurationMs);
  view.live_turn_metadata = {
    status,
    ...(Number.isFinite(duration) && duration >= 0
      ? { durationMs: duration }
      : {}),
    incomplete: meta?.turnIncomplete === true,
    ...(typeof meta?.turnErrorKind === "string"
      ? { errorKind: meta.turnErrorKind }
      : {}),
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
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isFileOperation(value: unknown): value is AcpFileOperation {
  return (
    value === "created" ||
    value === "modified" ||
    value === "deleted" ||
    value === "renamed" ||
    value === "read" ||
    value === "unknown"
  );
}

/**
 * Validate one ACP artifact without allowing malformed provider data into the
 * typed UI projection. Invalid optional fields are omitted; required fields
 * invalidate the reference.
 */
function parseArtifact(value: unknown): AcpArtifactReference | null | undefined {
  if (value === null) return null;
  if (!isRecord(value)) return undefined;
  if (
    typeof value.id !== "string" ||
    typeof value.media_type !== "string" ||
    !isFiniteNumber(value.size_bytes) ||
    value.size_bytes < 0
  ) {
    return undefined;
  }

  const artifact: AcpArtifactReference = {
    id: value.id,
    media_type: value.media_type,
    size_bytes: value.size_bytes,
  };
  if (value.path === null || typeof value.path === "string") {
    artifact.path = value.path;
  }
  if (value.sha256 === null || typeof value.sha256 === "string") {
    artifact.sha256 = value.sha256;
  }
  return artifact;
}

/** Validate and retain a single typed item from an ACP structured result. */
function parseToolResultItem(value: unknown): AcpToolResultItem | undefined {
  if (!isRecord(value) || typeof value.type !== "string") return undefined;

  switch (value.type) {
    case "text":
      return typeof value.text === "string"
        ? { type: "text", text: value.text }
        : undefined;
    case "diff": {
      if (typeof value.path !== "string" || typeof value.patch !== "string") {
        return undefined;
      }
      const item: Extract<AcpToolResultItem, { type: "diff" }> = {
        type: "diff",
        path: value.path,
        patch: value.patch,
      };
      if (value.old_path === null || typeof value.old_path === "string") {
        item.old_path = value.old_path;
      }
      return item;
    }
    case "file": {
      if (
        typeof value.path !== "string" ||
        !isFileOperation(value.operation)
      ) {
        return undefined;
      }
      const item: Extract<AcpToolResultItem, { type: "file" }> = {
        type: "file",
        path: value.path,
        operation: value.operation,
      };
      if (value.size_bytes === null || isFiniteNumber(value.size_bytes)) {
        item.size_bytes = value.size_bytes;
      }
      if (value.sha256 === null || typeof value.sha256 === "string") {
        item.sha256 = value.sha256;
      }
      return item;
    }
    case "command": {
      if (typeof value.command !== "string") return undefined;
      const item: Extract<AcpToolResultItem, { type: "command" }> = {
        type: "command",
        command: value.command,
      };
      if (value.exit_code === null || isFiniteNumber(value.exit_code)) {
        item.exit_code = value.exit_code;
      }
      if (typeof value.stdout === "string") item.stdout = value.stdout;
      if (typeof value.stderr === "string") item.stderr = value.stderr;
      if (value.duration_ms === null || isFiniteNumber(value.duration_ms)) {
        item.duration_ms = value.duration_ms;
      }
      return item;
    }
    case "image": {
      if (
        typeof value.media_type !== "string" ||
        typeof value.data !== "string"
      ) {
        return undefined;
      }
      const item: Extract<AcpToolResultItem, { type: "image" }> = {
        type: "image",
        media_type: value.media_type,
        data: value.data,
      };
      if (value.label === null || typeof value.label === "string") {
        item.label = value.label;
      }
      return item;
    }
    case "artifact": {
      const artifact = parseArtifact(value.artifact);
      return artifact && artifact !== null
        ? { type: "artifact", artifact }
        : undefined;
    }
    default:
      return undefined;
  }
}

/**
 * Recognize only the ACP structured-result shape. This intentionally stays
 * private to the store: raw tool output is untrusted provider data and the
 * public type module must remain a data-only contract.
 */
function parseStructuredToolResult(
  value: unknown,
): AcpStructuredToolResult | undefined {
  if (!isRecord(value) || typeof value.output !== "string") return undefined;

  const result: AcpStructuredToolResult = { output: value.output };
  if (typeof value.is_error === "boolean") result.is_error = value.is_error;
  if (typeof value.truncated === "boolean") result.truncated = value.truncated;
  if (value.original_bytes === null || isFiniteNumber(value.original_bytes)) {
    result.original_bytes = value.original_bytes;
  }
  if (Array.isArray(value.items)) {
    result.items = value.items
      .map(parseToolResultItem)
      .filter((item): item is AcpToolResultItem => item !== undefined);
  }
  if ("artifact" in value) {
    const artifact = parseArtifact(value.artifact);
    if (artifact !== undefined) result.artifact = artifact;
  }
  if (Array.isArray(value.extensions)) {
    result.extensions = value.extensions
      .filter(isRecord)
      .map((extension) => ({ ...extension }));
  }
  return result;
}

/** Derive the small, stable fields used by compact tool-row rendering. */
function structuredToolProjection(result: AcpStructuredToolResult): {
  path?: string;
  resultTitle?: string;
  durationMs?: number;
} {
  const items = result.items ?? [];
  const single = items.length === 1 ? items[0] : undefined;
  if (single?.type === "file") {
    return { path: single.path, resultTitle: single.operation };
  }
  if (single?.type === "diff") {
    return { path: single.path, resultTitle: "diff" };
  }

  const command = items.find((item) => item.type === "command");
  return command?.type === "command" &&
    command.duration_ms !== null &&
    command.duration_ms !== undefined &&
    Number.isFinite(command.duration_ms) &&
    command.duration_ms >= 0
    ? { durationMs: command.duration_ms }
    : {};
}

/**
 * ACP tool titles are provider data, not a second UI state machine.
 *
 * A few providers initially send a generic title (`tool`/`function`) and only
 * provide a useful kind or title in a later update. Keep a useful title once
 * observed and never replace it with a generic/empty update. This is the
 * canonical fallback used by both tool_call and tool_call_update.
 */
function isGenericToolTitle(value: string | null | undefined): boolean {
  const normalized = (value ?? "").trim().toLowerCase();
  return (
    normalized === "" ||
    normalized === "tool" ||
    normalized === "tools" ||
    normalized === "function" ||
    normalized === "unknown" ||
    normalized === "工具"
  );
}

function toolKindTitle(kind: string | null | undefined): string {
  return (kind ?? "").replace(/[_./-]+/g, " ").trim();
}

function mergeToolTitle(
  previous: string | undefined,
  incoming: string | null | undefined,
  kind: string | null | undefined,
): string {
  const current = previous?.trim() ?? "";
  const next = incoming?.trim() ?? "";
  if (next && !isGenericToolTitle(next)) return next;
  if (current && !isGenericToolTitle(current)) return current;
  const kindTitle = toolKindTitle(kind);
  if (kindTitle && !isGenericToolTitle(kindTitle)) return kindTitle;
  // Keep the original generic label only when it is all we have. An empty
  // title is also valid for an update-first placeholder; the UI can fill it
  // when a later ACP event provides the actual title.
  return next || current;
}

function isToolRunning(status: string): boolean {
  return (
    status === "pending" ||
    status === "in_progress" ||
    status === "running" ||
    status === ""
  );
}

function isTerminalToolStatus(status: string): boolean {
  return status === "completed" || status === "failed";
}

function cloneCompactFiles(
  files: ContextCompactMeta["files"],
): ContextCompactMeta["files"] {
  return files?.map((file) => ({ ...file }));
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
      if (!sourceAgentId) captureTurnMetadata(view, update);
      const segments = targetSegments(view, sourceAgentId);
      if (segments) appendText(segments, "content", textOf(update));
      break;
    }
    case "agent_thought_chunk": {
      if (!sourceAgentId) view.retry = null;
      if (!sourceAgentId) captureTurnMetadata(view, update);
      const segments = targetSegments(view, sourceAgentId);
      if (segments) appendText(segments, "thought", textOf(update));
      break;
    }
    case "tool_call": {
      if (!sourceAgentId) view.retry = null;
      const input = stringifyToolValue(update.rawInput);
      const status = update.status ?? "pending";
      const tool: MessageToolSegment = {
        kind: "tool",
        toolCallId: update.toolCallId,
        title: mergeToolTitle(undefined, update.title, update.kind),
        toolKind: update.kind,
        status,
        input,
        detail: input,
        streaming: isToolRunning(status),
        isError: status === "failed",
      };
      const segments = targetSegments(view, sourceAgentId);
      if (!segments) break;
      const existing = findToolIn(segments, update.toolCallId);
      if (existing) {
        const previousTitle = existing.title;
        const hadTerminalStatus = isTerminalToolStatus(existing.status);
        // Some transports deliver the result update before the duplicate
        // tool_call notification. That notification only enriches the
        // existing segment; it must not roll a terminal result back to a
        // running state or discard fields populated by the result update.
        if (!hadTerminalStatus) {
          existing.status = status;
          existing.streaming = isToolRunning(status);
          existing.isError = existing.isError || status === "failed";
        }
        existing.title = mergeToolTitle(previousTitle, update.title, update.kind);
        existing.toolKind = update.kind ?? existing.toolKind;
        if (input !== undefined) {
          existing.input = input;
          if (!hadTerminalStatus || existing.output === undefined) {
            existing.detail = input;
          }
        }
      } else {
        segments.push(tool);
      }
      break;
    }
    case "tool_call_update": {
      if (!sourceAgentId) view.retry = null;
      const segments = targetSegments(view, sourceAgentId);
      if (!segments) break;
      const existing = findToolIn(segments, update.toolCallId);
      const status = update.status ?? existing?.status ?? "in_progress";
      const structuredResult = parseStructuredToolResult(update.rawOutput);
      const output =
        structuredResult?.output ?? stringifyToolValue(update.rawOutput);
      const structuredProjection = structuredResult
        ? structuredToolProjection(structuredResult)
        : {};
      const structuredError = structuredResult?.is_error === true;
      if (!existing) {
        // ACP does not guarantee that tool_call precedes tool_call_update on
        // every transport. Retain a minimal segment so the later call can
        // enrich it in place instead of dropping the result.
        segments.push({
          kind: "tool",
          toolCallId: update.toolCallId,
          title: mergeToolTitle(undefined, update.title, update.kind),
          toolKind: update.kind,
          status,
          ...(output !== undefined ? { output, detail: output } : {}),
          ...(structuredResult ? { structuredResult } : {}),
          ...structuredProjection,
          streaming: isToolRunning(status),
          isError: status === "failed" || structuredError,
        });
        break;
      }

      const previousTitle = existing.title;
      if (update.status) existing.status = update.status;
      existing.title = mergeToolTitle(
        previousTitle,
        update.title,
        update.kind ?? existing.toolKind,
      );
      if (update.kind) existing.toolKind = update.kind;
      if (output !== undefined) {
        existing.output = output;
        existing.detail = output ?? existing.input;
      }
      if (structuredResult) {
        existing.structuredResult = structuredResult;
        existing.detail = structuredResult.output ?? existing.input;
        Object.assign(existing, structuredProjection);
      }
      existing.streaming = isToolRunning(existing.status);
      existing.isError =
        existing.isError || existing.status === "failed" || structuredError;
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
          summaryPreview: event.value.summary,
          files: cloneCompactFiles(event.value.files),
          skills: [...event.value.skills],
          microCleared: event.value.micro_cleared,
          strategy: event.value.strategy,
          outcome: event.value.outcome,
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
      view.last_error = { code: v.code, message: v.message };
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
      message: String(params.pending_tools.length),
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

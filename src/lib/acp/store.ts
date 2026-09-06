/** ACP 事件归约器：把 acp://* 通知归约为当前 UI 会话视图。 */

import type {
  AcpDeliveryEnvelope,
  AcpToolCallContent,
  GoalRecordDto,
  KeenCodeEvent,
  SessionUpdate,
} from "./events";
import { parseAttachmentsFromContent, type Attachment } from "../attachments";
import {
  compactMessageSegments,
  deriveFieldsFromSegments,
  type ContextCompactMeta,
  type MessageToolSegment,
  type MessageFileChange,
  type MessageSegment,
} from "../session";
import type {
  TurnLatencySummary,
} from "../turnLatency";
import type { AgentNicknameRef } from "../agentNicknames";
import type {
  AcpArtifactReference,
  AcpFileOperation,
  AcpRetryProjection,
  AcpStructuredToolResult,
  AcpSystemNotificationLevel,
  AcpToolResultItem,
} from "./types";
import { parseFileChangeResourceLink } from "./fileChanges";
import { toolCompletionStatusOf } from "./events";

export interface AcpHistoryMessage {
  /** 消息角色。 */
  role: string;
  /** 后端权威消息标识；回退操作必须使用该标识而不是正文或位置。 */
  messageId?: string;
  /** 消息所属的稳定 Turn 标识；无 messageId 时用于阻止跨 Turn 合并。 */
  turnId?: string;
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
  /** Assistant Turn 实际使用的模型；投影时关联到对应用户消息。 */
  model?: string;
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
  /** Agent 定义中面向用户的类型说明。 */
  agent_description?: string;
  /** 主 Agent 下发任务正文的首行短标题。 */
  task_title: string;
  nickname: AgentNicknameRef | null;
  /** 主 Agent 委派给该子 Agent 的原始任务。 */
  prompt: string;
  /** 子 Agent 的界面投影状态；中断不是执行失败。 */
  status: "running" | "done" | "interrupted" | "failed";
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
  /** 完整历史是否已由前端消费；空历史也必须能区分于尚未加载。 */
  loaded: boolean;
  /** 控制响应已确认的历史投递终点；null 表示本次 Host 尚未返回水位。 */
  throughDeliverySequence: number | null;
  /** 已确认消费的权威 Journal 序号。 */
  after: number | null;
  /** 最近一次 replay 观察到的 Journal 尾部。 */
  throughJournalSequence: number;
  /** 当前水位之后是否仍有分页。 */
  hasMore: boolean;
  /** 是否正在用标准 load 与类型化 replay 重建投影。 */
  restoring: boolean;
}

/** 两类投递信封共享的 Session 顺序水位。 */
export interface AcpDeliveryProjection {
  /** 最近已归约的投递序号；首次附着前为空。 */
  lastSequence: number | null;
  /** 检测到缺口后是否冻结后续增量。 */
  frozen: boolean;
  /** 缺口期望的下一个序号。 */
  expectedSequence: number | null;
  /** 首次越过缺口到达的实际序号。 */
  receivedSequence: number | null;
}

export interface AcpToolSearchProjection {
  query: string;
  tools: unknown[];
  total: number;
  truncated: boolean;
  catalog_revision: number;
}

export interface AcpSessionView {
  /** 根 Session 稳定标识。 */
  session_id: string;
  /** Session 当前绑定的项目绝对路径。 */
  project_path: string | null;
  /** "attached" | "streaming" | "idle" 等展示状态。 */
  status: string;
  /** 当前 Turn 按 ACP 到达顺序维护的唯一时间线。 */
  live_segments: MessageSegment[];
  /** 当前根 Agent 正在归约的 Turn；没有运行中根 Turn 时为空。 */
  active_root_turn_id: string | null;
  /** Session 当前是否启用 Runtime 强制只读 PlanGuard。 */
  plan_mode: boolean;
  history: AcpHistoryMessage[];
  /** 两类信封共同使用的投递顺序水位。 */
  delivery: AcpDeliveryProjection;
  /** 已提交终态的 Turn，用于阻止迟到增量改写投影。 */
  terminal_turns: Record<string, "completed" | "failed" | "cancelled">;
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
    model?: string;
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
    active_root_turn_id: null,
    plan_mode: false,
    history: [],
    delivery: {
      lastSequence: null,
      frozen: false,
      expectedSequence: null,
      receivedSequence: null,
    },
    terminal_turns: {},
    last_error: null,
    goal: { revision: 0, goal: null },
    todos: { revision: 0, items: [] },
    subagents: [],
    replay: {
      loaded: false,
      throughDeliverySequence: null,
      after: null,
      throughJournalSequence: 0,
      hasMore: false,
      restoring: false,
    },
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
 * 标准 `turn_started` 到达前本地发送已经建立可见忙碌态，因此先清理只属于
 * 上一轮的瞬时错误；权威 Turn 身份仍以后续类型化事件为准。
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

/** 从标准更新的命名空间元数据读取 Runtime 资源消息标识。 */
function messageIdOf(value: unknown): string | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const meta = (value as { _meta?: unknown })._meta;
  if (!meta || typeof meta !== "object" || Array.isArray(meta)) return undefined;
  const messageId = (meta as Record<string, unknown>)["keencode/messageId"];
  return typeof messageId === "string" && messageId.length > 0
    ? messageId
    : undefined;
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

/** 读取 Runtime 写入标准 Plan `_meta` 的权威 Todo revision。 */
function todoRevisionOf(
  update: Extract<SessionUpdate, { sessionUpdate: "plan" }>,
): number | null {
  const candidate = update._meta?._keencode;
  const keencode = isRecord(candidate) ? candidate : null;
  const revision = keencode?.todoRevision;
  return typeof revision === "number" && Number.isSafeInteger(revision) &&
      revision >= 0
    ? revision
    : null;
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

/** 提取标准工具内容中的本次精确快照；省略 content 不得清空既有状态。 */
function standardFileChanges(
  content: AcpToolCallContent[] | undefined,
  sessionId: string,
): MessageFileChange[] | undefined {
  return content?.flatMap((item): MessageFileChange[] => {
    if (item.type === "diff") {
      return [{
        path: item.path,
        oldText: item.oldText ?? null,
        newText: item.newText,
      }];
    }
    if (item.type !== "content" || item.content.type !== "resource_link") {
      return [];
    }
    const reference = parseFileChangeResourceLink(item.content, sessionId);
    return reference
      ? [{ path: reference.path, reference }]
      : [];
  });
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

/** 读取当前原生工具结果的文本投影；必须匹配调用身份，正文不在传输中重复存储。 */
function nativeToolResultText(value: unknown, toolCallId: string): string | undefined {
  if (!isRecord(value) || value.toolCallId !== toolCallId ||
      typeof value.isError !== "boolean" || !Array.isArray(value.content)) return undefined;
  const texts: string[] = [];
  for (const part of value.content) {
    if (!isRecord(part)) return undefined;
    if (part.type === "text" && typeof part.text === "string") texts.push(part.text);
    // 非文本引用仍留在详情中，不能为了可读摘要把图片或大结果引用静默丢掉。
    else if (part.type === "image" || part.type === "artifact") texts.push(JSON.stringify(part));
    else return undefined;
  }
  return texts.length ? texts.join("\n") : undefined;
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

/** 仅把已由权威 `agent_spawned` 登记的身份路由到子 Agent 时间线。 */
export function resolveChildAgentId(
  view: AcpSessionView,
  sourceAgentId: string | null | undefined,
): string | undefined {
  if (!sourceAgentId) return undefined;
  return view.subagents.some((agent) => agent.agent_id === sourceAgentId)
    ? sourceAgentId
    : undefined;
}

/** 把根 Agent 当前 Turn 固化进历史并清空实时缓冲。 */
export function commitLiveTurnToHistory(
  view: AcpSessionView,
  options?: {
    /** 标准更新缺失时由本地乐观消息补入的用户正文。 */
    userContent?: string;
    /** 本轮思考耗时，单位毫秒。 */
    thinkingDurationMs?: number;
    /** 本轮低延迟链路观测。 */
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
  if (userContent &&
    !(lastHistoryMessage?.role === "user" &&
      (lastHistoryMessage.content === userContent ||
        lastHistoryDisplayContent === userContent))) {
    view.history.push({ role: "user", content: userContent });
  }
  const segments = compactMessageSegments(view.live_segments);
  const fields = deriveFieldsFromSegments(segments);
  const turnMetadata = view.live_turn_metadata;
  if (segments.length > 0 || turnMetadata) {
    const turnId = view.active_root_turn_id ?? options?.turnMetrics?.turnId;
    view.history.push({
      role: "assistant",
      ...(turnId ? { turnId } : {}),
      content: fields.content,
      ...(fields.thought ? { thought: fields.thought } : {}),
      segments,
      ...((turnMetadata?.durationMs ?? options?.thinkingDurationMs) != null
        ? { thinkingDurationMs: turnMetadata?.durationMs ?? options?.thinkingDurationMs }
        : {}),
      ...(turnMetadata
        ? {
            turnStatus: turnMetadata.status,
            turnIncomplete: turnMetadata.incomplete,
            turnErrorKind: turnMetadata.errorKind,
          }
        : {}),
      ...(options?.turnMetrics ? { turnMetrics: options.turnMetrics } : {}),
      ...(turnMetadata?.model || options?.model
        ? { model: turnMetadata?.model ?? options?.model }
        : {}),
    });
  }
  view.live_segments = [];
  view.live_turn_metadata = null;
}

/** 归约一条已通过严格信封和顺序门禁的标准 SessionUpdate。 */
function reduceSessionUpdate(
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
      const messageId = messageIdOf(update);
      const turnId = view.active_root_turn_id;
      if (text) {
        const last = view.history.at(-1);
        const sameMessage =
          messageId !== undefined && last?.messageId === messageId;
        const sameTurnWithoutMessageId =
          messageId === undefined &&
          last?.messageId === undefined &&
          turnId !== null &&
          last?.turnId === turnId;
        if (
          !sourceAgentId &&
          last?.role === "user" &&
          (sameMessage || sameTurnWithoutMessageId)
        ) {
          if (messageId !== undefined) last.messageId = messageId;
          last.content += text;
        } else if (!sourceAgentId) {
          view.history.push({
            role: "user",
            ...(messageId === undefined ? {} : { messageId }),
            ...(turnId === null ? {} : { turnId }),
            content: text,
          });
        }
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
      const status = update.status ?? "pending";
      const completionStatus = toolCompletionStatusOf(update);
      const tool: MessageToolSegment = {
        kind: "tool",
        toolCallId: update.toolCallId,
        title: mergeToolTitle(undefined, update.title, update.kind),
        toolKind: update.kind,
        status,
        input,
        ...(completionStatus ? { completionStatus } : {}),
        detail: input,
        ...(update.content !== undefined
          ? { fileChanges: standardFileChanges(update.content, view.session_id) }
          : {}),
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
          if (completionStatus) existing.completionStatus = completionStatus;
          existing.streaming = isToolRunning(status);
          existing.isError = existing.isError || status === "failed";
        }
        existing.title = mergeToolTitle(previousTitle, update.title, update.kind);
        existing.toolKind = update.kind ?? existing.toolKind;
        if (update.content !== undefined &&
          (!hadTerminalStatus || existing.fileChanges === undefined)) {
          existing.fileChanges = standardFileChanges(update.content, view.session_id);
        }
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
      const completionStatus = toolCompletionStatusOf(update);
      const structuredResult = parseStructuredToolResult(update.rawOutput);
      const standardTexts = update.content
        ?.flatMap((item) => item.type === "content" && item.content.type === "text"
          ? [item.content.text]
          : []);
      const standardOutput = standardTexts?.length ? standardTexts.join("\n") : undefined;
      const output =
        standardOutput ??
        nativeToolResultText(update.rawOutput, update.toolCallId) ??
        structuredResult?.output ??
        stringifyToolValue(update.rawOutput);
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
          ...(completionStatus ? { completionStatus } : {}),
          ...(output !== undefined ? { output, detail: output } : {}),
          ...(structuredResult ? { structuredResult } : {}),
          ...structuredProjection,
          ...(update.content !== undefined
            ? { fileChanges: standardFileChanges(update.content, view.session_id) }
            : {}),
          streaming: isToolRunning(status),
          isError: status === "failed" || structuredError,
        });
        break;
      }

      const previousTitle = existing.title;
      if (update.status) existing.status = update.status;
      if (completionStatus) existing.completionStatus = completionStatus;
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
        existing.detail = output ?? existing.input;
        Object.assign(existing, structuredProjection);
      }
      if (update.content !== undefined) {
        existing.fileChanges = standardFileChanges(update.content, view.session_id);
      }
      existing.streaming = isToolRunning(existing.status);
      existing.isError =
        existing.isError || existing.status === "failed" || structuredError;
      break;
    }
    case "plan": {
      if (!sourceAgentId) view.retry = null;
      const revision = todoRevisionOf(update);
      if (revision !== null) {
        // 权威 Plan 可能在实时投影和 replay 投影之间交错到达，不能用本地
        // 到达次数推导 revision；Runtime 已在 Journal 归约时冻结最终版本。
        view.todos.revision = revision;
      } else {
        // 未携带 KeenCode 扩展的标准 ACP Plan 仍保持可展示的本地版本。
        view.todos.revision += 1;
      }
      view.todos.items = update.entries.map((e) => ({
        content: e.content,
        status: e.status,
      }));
      break;
    }
    case "usage_update":
      break;
    case "session_info_update": {
      if (typeof update.title === "string") view.title = update.title;
      break;
    }
    case "current_mode_update": {
      if (update.currentModeId === "plan") view.plan_mode = true;
      if (update.currentModeId === "default") view.plan_mode = false;
      break;
    }
    case "config_option_update":
    case "available_commands_update":
      break;
  }
}

/** 从稳定 Agent 路径提取不依赖厂商字段的显示名称。 */
function agentDisplayName(agentPath: string): string {
  const parts = agentPath.split("/").filter(Boolean);
  return parts.at(-1) ?? agentPath;
}

/** 将完整生命周期状态投影到当前四态子 Agent 视图。 */
function projectAgentStatus(
  status: Extract<KeenCodeEvent, { type: "agent_status_changed" }>["status"],
): AcpSubagentInfo["status"] {
  switch (status) {
    case "completed":
    case "stopped":
      return "done";
    case "interrupted":
      return "interrupted";
    case "failed":
      return "failed";
    case "pending":
    case "running":
    case "waiting":
      return "running";
  }
}

/** 归约一条已通过严格信封和顺序门禁的 KeenCode 生命周期事件。 */
function reduceKeenCodeEvent(
  view: AcpSessionView,
  event: KeenCodeEvent,
  turnId: string | undefined,
  sourceAgentId: string | undefined,
  occurredAtMs: number,
): void {
  const childAgentId = resolveChildAgentId(view, sourceAgentId);
  switch (event.type) {
    case "turn_started": {
      if (childAgentId) {
        const agent = view.subagents.find((item) => item.agent_id === childAgentId);
        if (agent) {
          agent.status = "running";
          agent.started_at = occurredAtMs;
          agent.stopped_at = null;
          agent.result = null;
        }
        break;
      }
      if (event.parentTurnId !== undefined || !turnId) break;
      if (view.live_segments.length > 0 || view.live_turn_metadata) {
        commitLiveTurnToHistory(view);
      }
      view.active_root_turn_id = turnId;
      view.status = "streaming";
      view.turn_started_at = occurredAtMs;
      view.last_error = null;
      view.retry = null;
      break;
    }
    case "turn_completed":
    case "turn_cancelled":
    case "turn_failed": {
      if (!turnId || view.terminal_turns[turnId]) break;
      const status = event.type === "turn_completed"
        ? "completed"
        : event.type === "turn_cancelled"
          ? "cancelled"
          : "failed";
      view.terminal_turns[turnId] = status;
      if (childAgentId) {
        const agent = view.subagents.find((item) => item.agent_id === childAgentId);
        if (agent) {
          agent.status = status === "completed"
            ? "done"
            : status === "cancelled"
              ? "interrupted"
              : "failed";
          agent.stopped_at = occurredAtMs;
          if (event.type === "turn_failed") agent.result = event.message;
        }
        break;
      }
      if (view.active_root_turn_id !== turnId) break;
      const durationMs = view.turn_started_at == null
        ? undefined
        : Math.max(0, occurredAtMs - view.turn_started_at);
      view.live_turn_metadata = {
        status,
        ...(durationMs !== undefined ? { durationMs } : {}),
        incomplete: status !== "completed",
        ...(event.type === "turn_failed" ? { errorKind: event.failureKind } : {}),
      };
      if (event.type === "turn_failed") {
        view.last_error = { code: event.failureKind, message: event.message };
      }
      commitLiveTurnToHistory(view);
      view.active_root_turn_id = null;
      view.turn_started_at = null;
      view.status = "idle";
      view.retry = null;
      break;
    }
    case "agent_spawned": {
      view.subagents = view.subagents.filter((item) => item.agent_id !== event.agentId);
      const taskTitle = event.task.split(/\r?\n/u, 1)[0]?.slice(0, 120).trim() || event.task;
      view.subagents.push({
        agent_id: event.agentId,
        agent_name: agentDisplayName(event.agentPath),
        task_title: taskTitle,
        prompt: event.task,
        nickname: null,
        status: "running",
        is_background: true,
        started_at: occurredAtMs,
        stopped_at: null,
        result: null,
        segments: [],
      });
      break;
    }
    case "agent_status_changed": {
      const agent = view.subagents.find((item) => item.agent_id === event.agentId);
      if (!agent) break;
      agent.status = projectAgentStatus(event.status);
      if (agent.status !== "running") agent.stopped_at = occurredAtMs;
      break;
    }
    case "context_compaction_started": {
      view.compacting = true;
      break;
    }
    case "context_compaction_completed": {
      view.compacting = false;
      view.history.push({
        role: "tool",
        content: "context_compact",
        marker: "context_compact",
        compactMeta: {
          trigger: "auto",
          tokensAfter: event.estimatedTokens,
        },
      });
      break;
    }
    case "context_compaction_failed": {
      view.compacting = false;
      break;
    }
    case "recovery_state_changed": {
      view.replay.restoring = event.state === "pending" || event.state === "replaying";
      if (event.state === "ready" && view.status === "connecting") {
        view.status = "ready";
      }
      if (event.state === "failed") {
        view.delivery.frozen = true;
        view.last_error = {
          code: "session_recovery_failed",
          message: "Session 恢复失败",
        };
      }
      break;
    }
    case "goal_changed": {
      const goal = view.goal.goal;
      const status = event.status;
      view.goal = {
        revision: event.revision,
        goal: !event.goalId
          ? null
          : goal?.id === event.goalId &&
              (status === "active" || status === "completed" || status === "blocked")
            ? { ...goal, status }
            : null,
      };
      break;
    }
    case "system_notification": {
      view.history.push({
        role: "tool",
        content: event.message,
        marker: "system_notification",
        systemNotificationLevel: event.level,
      });
      break;
    }
    case "model_retry_scheduled": {
      view.retry = {
        attempt: event.attempt,
        maxAttempts: event.maxAttempts,
        delayMs: event.delayMs,
        reason: event.message,
      };
      break;
    }
    case "background_task_completed": {
      if (event.taskKind !== "agent" || !event.agentId) break;
      const agent = view.subagents.find((item) => item.agent_id === event.agentId);
      if (!agent) break;
      agent.status = event.status === "succeeded"
        ? "done"
        : event.status === "cancelled"
          ? "interrupted"
          : "failed";
      agent.stopped_at = occurredAtMs;
      agent.result = event.summary ?? null;
      break;
    }
    case "agent_message_queued":
    case "model_first_stream_observed":
      break;
  }
}

/** 单条投递经过共享水位门禁后的结果。 */
export type AcpDeliveryReduction =
  | {
      /** 信封已按唯一 Reducer 归约。 */
      status: "applied";
      /** 已登记的子 Agent 来源；根 Agent 或 Session 级事件为空。 */
      childAgentId?: string;
      /** 终态后的迟到 SessionUpdate 是否只推进了水位。 */
      ignoredTerminalUpdate: boolean;
    }
  | { status: "duplicate" }
  | { status: "stale_generation" }
  | { status: "frozen" }
  | { status: "gap"; expectedSequence: number; receivedSequence: number };

/**
 * 按每 Session 共享的 `deliverySequence` 归约两类信封。
 * 实时与 replay 必须调用同一入口；检测到缺口后立即冻结增量。
 */
export function reduceDeliveryEnvelope(
  view: AcpSessionView,
  envelope: AcpDeliveryEnvelope,
): AcpDeliveryReduction {
  if (envelope.sessionId !== view.session_id) {
    view.delivery.frozen = true;
    return { status: "frozen" };
  }
  if (view.delivery.frozen) return { status: "frozen" };
  if (
    view.replay.restoring && view.delivery.lastSequence === null &&
    envelope.deliverySequence !== 1
  ) {
    return { status: "stale_generation" };
  }
  const previous = view.delivery.lastSequence;
  if (previous !== null && envelope.deliverySequence <= previous) {
    return { status: "duplicate" };
  }
  const expectedSequence = previous === null ? 1 : previous + 1;
  if (envelope.deliverySequence !== expectedSequence) {
    view.delivery.frozen = true;
    view.delivery.expectedSequence = expectedSequence;
    view.delivery.receivedSequence = envelope.deliverySequence;
    return {
      status: "gap",
      expectedSequence,
      receivedSequence: envelope.deliverySequence,
    };
  }

  view.delivery.lastSequence = envelope.deliverySequence;
  const childAgentId = resolveChildAgentId(view, envelope.sourceAgentId);
  let ignoredTerminalUpdate = false;
  if ("update" in envelope) {
    ignoredTerminalUpdate = Boolean(
      envelope.turnId && view.terminal_turns[envelope.turnId],
    );
    if (!ignoredTerminalUpdate) {
      reduceSessionUpdate(view, envelope.update, childAgentId);
    }
  } else {
    reduceKeenCodeEvent(
      view,
      envelope.event,
      envelope.turnId,
      envelope.sourceAgentId,
      envelope.occurredAtMs,
    );
    if (envelope.journalSequence !== undefined) {
      view.replay.after = Math.max(view.replay.after ?? 0, envelope.journalSequence);
    }
  }
  return {
    status: "applied",
    ...(childAgentId ? { childAgentId } : {}),
    ignoredTerminalUpdate,
  };
}

/** 清空不再可信的投影并进入标准 load/replay 恢复窗口。 */
export function beginSessionRecovery(view: AcpSessionView): void {
  const sessionId = view.session_id;
  const projectPath = view.project_path;
  Object.assign(view, emptySession(sessionId));
  view.project_path = projectPath;
  view.status = "connecting";
  view.replay.restoring = true;
}

/** 以最后的 replay 控制响应完成恢复，不伪造投递或 Journal 序号。 */
export function completeSessionRecovery(view: AcpSessionView): void {
  view.replay.restoring = false;
  view.replay.loaded = true;
  if (!view.delivery.frozen && view.status === "connecting") {
    view.status = "ready";
  }
}

/** 恢复失败时保持增量冻结，并记录不含厂商正文的稳定错误。 */
export function failSessionRecovery(view: AcpSessionView, message: string): void {
  view.replay.restoring = false;
  view.replay.loaded = false;
  view.delivery.frozen = true;
  view.status = "ready";
  view.last_error = { code: "session_recovery_failed", message };
}

/** 归约 `keencode/goal/get` 查询结果（全量替换）。 */
export function reduceGoalSnapshot(
  view: AcpSessionView,
  revision: number,
  goal: GoalRecordDto | null,
): void {
  view.goal = { revision, goal };
}

/** 归约 `keencode/session/replay` 控制响应并推进 Journal 水位。 */
export function reduceReplayResult(
  view: AcpSessionView,
  result: {
    /** 被重放的 Session。 */
    sessionId: string;
    /** 本页开始前的确认水位。 */
    startAfter: number;
    /** 本页完成后的确认水位。 */
    nextAfter: number;
    /** 本次读取观察到的 Journal 尾部。 */
    throughJournalSequence: number;
    /** 已实际投递的最后历史信封序号；不代表前端已经处理。 */
    throughDeliverySequence: number;
    /** 本页实际投递事件数。 */
    replayedEvents: number;
    /** 是否还有下一页。 */
    hasMore: boolean;
  },
): void {
  if (result.sessionId !== view.session_id) return;
  view.replay.after = result.nextAfter > 0 ? result.nextAfter : null;
  view.replay.throughJournalSequence = result.throughJournalSequence;
  view.replay.throughDeliverySequence = result.throughDeliverySequence;
  view.replay.hasMore = result.hasMore;
}

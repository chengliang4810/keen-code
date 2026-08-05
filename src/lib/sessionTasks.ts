/**
 * Active / recent agent tool tasks for the session Tasks panel (L05).
 *
 * Source of truth: live + journal `tool_step` rows already produced from ACP
 * `session://tool` events (toolCallId, title, kind, status, path, detail).
 * There is no separate ACP "task list" API — do not invent one.
 */

import type { ChatMessage, MessageToolSegment } from "./session";
import {
  isToolStepMessage,
  parseToolStepContent,
  toolStepDisplayTitle,
} from "./session";

/** Normalized UI status for a tool task row. */
export type AgentTaskStatus =
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export interface AgentTask {
  /** Stable tool call id from ACP / host. */
  id: string;
  /** Human label (title / command / path). */
  name: string;
  /** Raw tool kind from the current ACP projection. */
  kind: string;
  status: AgentTaskStatus;
  /** Optional command / query snippet. */
  detail?: string;
  /** Optional path from tool payload. */
  path?: string;
  /** ISO timestamp of last update when available. */
  updatedAt?: string;
  /**
   * Tools that often outlive a single stream tick (agents and background
   * commands). Used only for grouping / badge — not a separate protocol type.
   */
  longRunning: boolean;
}

/** 任务面板消费的统一工具投影。 */
export interface ToolTaskSource {
  /** ACP 工具调用标识。 */
  id: string;
  /** 工具展示标题。 */
  title: string;
  /** 工具类型。 */
  kind: string;
  /** 工具原始状态。 */
  status: string;
  /** 工具详情。 */
  detail?: string;
  /** 工具目标路径。 */
  path?: string;
  /** 工具是否仍在流式执行。 */
  streaming?: boolean;
  /** 工具是否失败。 */
  isError?: boolean;
  /** 来源消息更新时间。 */
  updatedAt?: string;
}

/** Max completed/failed/cancelled rows kept after the active ones. */
export const SESSION_TASKS_RECENT_LIMIT = 24;

const RUNNING_STATUSES = new Set([
  "in_progress",
  "pending",
  "running",
  "",
]);

const FAILED_STATUSES = new Set(["failed", "error", "rejected"]);

const CANCELLED_STATUSES = new Set(["cancelled", "canceled"]);

/**
 * 当前运行时中通常表示多步骤或后台工作的工具类型。
 * Matching is advisory for UI emphasis; every tool_step can still appear as a task.
 */
const LONG_RUNNING_KINDS = new Set([
  "agent",
  "subagent",
  "bash",
  "execute",
  "background",
]);

export function isRunningToolStatus(status: string | null | undefined): boolean {
  const s = (status || "").toLowerCase().trim();
  return RUNNING_STATUSES.has(s);
}

export function normalizeTaskStatus(
  status: string | null | undefined,
  streaming?: boolean,
): AgentTaskStatus {
  if (streaming) return "running";
  const s = (status || "").toLowerCase().trim();
  if (!s || RUNNING_STATUSES.has(s)) return "running";
  if (FAILED_STATUSES.has(s)) return "failed";
  if (CANCELLED_STATUSES.has(s)) return "cancelled";
  if (
    s === "completed" ||
    s === "complete" ||
    s === "done" ||
    s === "success"
  ) {
    return "completed";
  }
  // Unknown terminal-ish labels → treat as completed for display.
  return "completed";
}

export function isLongRunningToolKind(kind: string | null | undefined): boolean {
  const k = (kind || "").toLowerCase().trim().replace(/-/g, "_");
  if (!k) return false;
  if (LONG_RUNNING_KINDS.has(k)) return true;
  if (k.includes("agent")) return true;
  if (k.includes("background")) return true;
  return false;
}

function resolveKind(m: ChatMessage): string {
  if (m.toolKind?.trim()) return m.toolKind.trim();
  if (m.content?.startsWith("tool_step|")) {
    return parseToolStepContent(m.content)?.kind?.trim() || "";
  }
  return "";
}

function resolveStatusRaw(m: ChatMessage): string {
  if (m.toolStatus?.trim()) return m.toolStatus.trim();
  if (m.content?.startsWith("tool_step|")) {
    return parseToolStepContent(m.content)?.status?.trim() || "";
  }
  return m.streaming ? "in_progress" : "completed";
}

function resolveDetail(m: ChatMessage): string | undefined {
  if (m.toolDetail?.trim()) return m.toolDetail.trim();
  if (m.content?.startsWith("tool_step|")) {
    return parseToolStepContent(m.content)?.detail?.trim() || undefined;
  }
  return undefined;
}

function resolvePath(m: ChatMessage): string | undefined {
  if (m.toolPath?.trim()) return m.toolPath.trim();
  if (m.content?.startsWith("tool_step|")) {
    return parseToolStepContent(m.content)?.path?.trim() || undefined;
  }
  return undefined;
}

function resolveId(m: ChatMessage): string {
  if (m.toolCallId?.trim()) return m.toolCallId.trim();
  if (m.id.startsWith("tool-")) return m.id.slice(5);
  return m.id;
}

/** 将 assistant 时间线中的工具段转换为任务数据源。 */
function sourceFromToolSegment(
  segment: MessageToolSegment,
  updatedAt?: string,
): ToolTaskSource {
  return {
    id: segment.toolCallId?.trim() || "",
    title: segment.title?.trim() || "",
    kind: segment.toolKind?.trim() || "",
    status: segment.status?.trim() || "",
    detail: segment.detail?.trim() || undefined,
    path: segment.path?.trim() || undefined,
    streaming: segment.streaming,
    isError: segment.isError,
    updatedAt,
  };
}

/** 将真实独立 tool_step 行转换为任务数据源。 */
function sourceFromToolMessage(m: ChatMessage): ToolTaskSource | null {
  if (!isToolStepMessage(m)) return null;
  const id = resolveId(m);
  if (!id) return null;
  return {
    id,
    title: toolStepDisplayTitle(m),
    kind: resolveKind(m),
    status: resolveStatusRaw(m),
    detail: resolveDetail(m),
    path: resolvePath(m),
    streaming: m.streaming,
    isError: m.isError,
    updatedAt: m.createdAt,
  };
}

/**
 * 收集工具调用：assistant 的 MessageToolSegment 是唯一首选投影；只有没有
 * 对应 assistant 段的 tool_step 行才作为真实独立工具保留。
 */
export function collectToolTaskSources(
  messages: ChatMessage[],
  from = 0,
): ToolTaskSource[] {
  const assistantToolIds = new Set<string>();
  for (let index = from; index < messages.length; index++) {
    const message = messages[index]!;
    if (message.role !== "assistant") continue;
    for (const segment of message.segments ?? []) {
      if (segment.kind !== "tool") continue;
      const id = segment.toolCallId?.trim() || "";
      if (id) assistantToolIds.add(id);
    }
  }

  const sources: ToolTaskSource[] = [];
  for (let index = from; index < messages.length; index++) {
    const message = messages[index]!;
    if (message.role === "assistant") {
      for (const segment of message.segments ?? []) {
        if (segment.kind !== "tool") continue;
        const source = sourceFromToolSegment(segment, message.createdAt);
        if (source.id) sources.push(source);
      }
      continue;
    }
    const source = sourceFromToolMessage(message);
    if (source && !assistantToolIds.has(source.id)) sources.push(source);
  }
  return sources;
}

/** 将统一工具数据源转换为任务面板行。 */
function taskFromToolSource(source: ToolTaskSource): AgentTask {
  const status = normalizeTaskStatus(
    source.isError ? "failed" : source.status,
    source.streaming,
  );
  return {
    id: source.id,
    name: source.title || source.kind.replace(/_/g, " ") || source.id,
    kind: source.kind,
    status,
    detail: source.detail,
    path: source.path,
    updatedAt: source.updatedAt,
    longRunning: isLongRunningToolKind(source.kind),
  };
}

/** Build one task row from a tool_step chat message. */
export function taskFromToolMessage(m: ChatMessage): AgentTask | null {
  const source = sourceFromToolMessage(m);
  return source ? taskFromToolSource(source) : null;
}

export interface CollectSessionTasksOptions {
  /** Cap on non-running rows (default SESSION_TASKS_RECENT_LIMIT). */
  recentLimit?: number;
  /**
   * When true (default), prefer tools after the last user message.
   * Still-running tools from earlier in the list are always kept.
   */
  currentTurnOnly?: boolean;
}

/**
 * Derive active + recent tool tasks from session messages.
 * Running first (stream order), then recent terminal rows (newest first).
 */
export function collectSessionTasks(
  messages: ChatMessage[],
  options: CollectSessionTasksOptions = {},
): AgentTask[] {
  const recentLimit = options.recentLimit ?? SESSION_TASKS_RECENT_LIMIT;
  const currentTurnOnly = options.currentTurnOnly !== false;

  let from = 0;
  if (currentTurnOnly) {
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i]!.role === "user") {
        from = i + 1;
        break;
      }
    }
  }

  const byId = new Map<string, AgentTask>();
  // Always scan full list for still-running tools (turn boundary can lag).
  for (const source of collectToolTaskSources(messages)) {
    const task = taskFromToolSource(source);
    if (task.status === "running") {
      byId.set(task.id, task);
    }
  }
  // Current-turn (or full) scan for terminal rows — last write wins.
  for (const source of collectToolTaskSources(messages, from)) {
    const task = taskFromToolSource(source);
    if (task.status === "running") {
      byId.set(task.id, task);
      continue;
    }
    const prev = byId.get(task.id);
    if (prev?.status === "running") continue;
    byId.set(task.id, task);
  }

  const all = Array.from(byId.values());
  const running = all.filter((t) => t.status === "running");
  const done = all
    .filter((t) => t.status !== "running")
    .sort((a, b) => {
      const ta = a.updatedAt || "";
      const tb = b.updatedAt || "";
      return tb.localeCompare(ta);
    })
    .slice(0, Math.max(0, recentLimit));

  return [...running, ...done];
}

export function countRunningTasks(tasks: AgentTask[]): number {
  return tasks.reduce((n, t) => (t.status === "running" ? n + 1 : n), 0);
}

export function filterSessionTasks(
  tasks: AgentTask[],
  query: string,
): AgentTask[] {
  const q = query.trim().toLowerCase();
  if (!q) return tasks;
  return tasks.filter(
    (t) =>
      t.name.toLowerCase().includes(q) ||
      t.kind.toLowerCase().includes(q) ||
      (t.detail || "").toLowerCase().includes(q) ||
      (t.path || "").toLowerCase().includes(q) ||
      t.id.toLowerCase().includes(q),
  );
}

/** Status message keys under activity.* for existing i18n. */
export function taskStatusMessageKey(
  status: AgentTaskStatus,
):
  | "activity.running"
  | "activity.done"
  | "activity.failed"
  | "activity.cancelled" {
  switch (status) {
    case "running":
      return "activity.running";
    case "failed":
      return "activity.failed";
    case "cancelled":
      return "activity.cancelled";
    default:
      return "activity.done";
  }
}

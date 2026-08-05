/**
 * Per-turn activity view-model for transcript summary + tasks panel.
 * Single derivation path from tool_step ChatMessage rows.
 */

import type { ChatMessage } from "./session";
import {
  isToolStepMessage,
  parseToolStepContent,
  toolStepDisplayTitle,
} from "./session";
import { isEditToolKind } from "./sessionChanges";
import {
  isContextToolKind,
  summarizeToolDisplay,
  toolDetailTail,
} from "./toolDisplay";
import {
  collectToolTaskSources,
  isLongRunningToolKind,
  normalizeTaskStatus,
  type AgentTask,
  type AgentTaskStatus,
  type ToolTaskSource,
} from "./sessionTasks";

export type TurnActivityToolStatus = AgentTaskStatus;

export interface TurnActivityTool {
  id: string;
  name: string;
  kind: string;
  status: TurnActivityToolStatus;
  summary: string;
  detail?: string;
  /** Tail of detail for expanded view (last N lines). */
  detailTail?: string;
  path?: string;
  isError: boolean;
  isContext: boolean;
  longRunning: boolean;
  updatedAt?: string;
}

export type TurnActivitySegment =
  | { kind: "single"; tool: TurnActivityTool }
  | { kind: "context"; tools: TurnActivityTool[] };

export interface TurnActivity {
  /** Last user message id that anchors this turn (empty if none). */
  afterUserMessageId: string;
  tools: TurnActivityTool[];
  /** Collapsed segments with context grouping (≥3 consecutive context tools). */
  segments: TurnActivitySegment[];
  stepCount: number;
  errorCount: number;
  runningCount: number;
  /** Edit/write paths touched this turn. */
  modifiedPaths: string[];
  /** True when any tool failed — UI should default-expand. */
  shouldExpand: boolean;
}

const CONTEXT_GROUP_MIN = 3;

function lastUserIndex(messages: ChatMessage[]): number {
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i]!.role === "user") return i;
  }
  return -1;
}

function resolveId(m: ChatMessage): string {
  if (m.toolCallId?.trim()) return m.toolCallId.trim();
  if (m.id.startsWith("tool-")) return m.id.slice(5);
  return m.id;
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

/** Convert one tool_step message into a turn activity tool row. */
export function activityToolFromMessage(
  m: ChatMessage,
): TurnActivityTool | null {
  if (!isToolStepMessage(m)) return null;
  const id = resolveId(m);
  if (!id) return null;
  const kind = resolveKind(m);
  const status = normalizeTaskStatus(resolveStatusRaw(m), m.streaming);
  const name = toolStepDisplayTitle(m) || kind.replace(/_/g, " ") || id;
  const path = resolvePath(m);
  const detail = resolveDetail(m);
  const display = summarizeToolDisplay({
    kind,
    title: name,
    detail,
    path,
  });
  return {
    id,
    name,
    kind,
    status,
    summary: display.summary || name,
    detail,
    detailTail: detail ? toolDetailTail(detail, 8) : undefined,
    path,
    isError: status === "failed" || !!m.isError,
    isContext: isContextToolKind(kind, name),
    longRunning: isLongRunningToolKind(kind),
    updatedAt: m.createdAt,
  };
}

/** 将统一工具投影转换为单个 Turn 活动。 */
function activityToolFromSource(source: ToolTaskSource): TurnActivityTool {
  const status = normalizeTaskStatus(
    source.isError ? "failed" : source.status,
    source.streaming,
  );
  const name = source.title || source.kind.replace(/_/g, " ") || source.id;
  const display = summarizeToolDisplay({
    kind: source.kind,
    title: name,
    detail: source.detail,
    path: source.path,
  });
  return {
    id: source.id,
    name,
    kind: source.kind,
    status,
    summary: display.summary || name,
    detail: source.detail,
    detailTail: source.detail ? toolDetailTail(source.detail, 8) : undefined,
    path: source.path,
    isError: status === "failed" || !!source.isError,
    isContext: isContextToolKind(source.kind, name),
    longRunning: isLongRunningToolKind(source.kind),
    updatedAt: source.updatedAt,
  };
}

/**
 * Group consecutive context tools (≥3) into a single segment.
 * Threshold matches CodePilot / Opencode.
 */
export function groupActivitySegments(
  tools: TurnActivityTool[],
  minContext = CONTEXT_GROUP_MIN,
): TurnActivitySegment[] {
  const segments: TurnActivitySegment[] = [];
  let buf: TurnActivityTool[] = [];

  const flush = () => {
    if (buf.length >= minContext) {
      segments.push({ kind: "context", tools: buf });
    } else {
      for (const t of buf) segments.push({ kind: "single", tool: t });
    }
    buf = [];
  };

  for (const t of tools) {
    if (t.isContext) {
      buf.push(t);
    } else {
      flush();
      segments.push({ kind: "single", tool: t });
    }
  }
  flush();
  return segments;
}

export interface BuildTurnActivityOptions {
  /** When true (default), only tools after the last user message. */
  currentTurnOnly?: boolean;
}

/**
 * Build turn activity from session messages (after last user by default).
 * Stream order preserved for tools in the window.
 */
export function buildTurnActivity(
  messages: ChatMessage[],
  options: BuildTurnActivityOptions = {},
): TurnActivity {
  const currentTurnOnly = options.currentTurnOnly !== false;
  const lastUser = lastUserIndex(messages);
  const from = currentTurnOnly ? lastUser + 1 : 0;
  const afterUserMessageId =
    lastUser >= 0 ? messages[lastUser]!.id : "";

  const byId = new Map<string, TurnActivityTool>();
  const order: string[] = [];

  for (const source of collectToolTaskSources(messages, from)) {
    const tool = activityToolFromSource(source);
    if (!byId.has(tool.id)) order.push(tool.id);
    byId.set(tool.id, tool);
  }

  const tools = order.map((id) => byId.get(id)!).filter(Boolean);
  const errorCount = tools.filter((t) => t.status === "failed" || t.isError)
    .length;
  const runningCount = tools.filter((t) => t.status === "running").length;
  const pathSet = new Set<string>();
  for (const t of tools) {
    if (t.path && isEditToolKind(t.kind)) pathSet.add(t.path);
  }

  return {
    afterUserMessageId,
    tools,
    segments: groupActivitySegments(tools),
    stepCount: tools.length,
    errorCount,
    runningCount,
    modifiedPaths: Array.from(pathSet),
    shouldExpand: errorCount > 0,
  };
}

/** Map turn activity tools to AgentTask rows (tasks panel). */
export function tasksFromTurnActivity(
  activity: TurnActivity,
): AgentTask[] {
  return activity.tools.map((t) => ({
    id: t.id,
    name: t.name,
    kind: t.kind,
    status: t.status,
    detail: t.detail,
    path: t.path,
    updatedAt: t.updatedAt,
    longRunning: t.longRunning,
  }));
}

/**
 * Whether the turn left a recoverable narrative without assistant prose.
 * Used for tool-only turns.
 */
export function turnNeedsActivityNarrative(
  messages: ChatMessage[],
): boolean {
  const activity = buildTurnActivity(messages);
  if (activity.stepCount === 0) return false;
  const lastUser = lastUserIndex(messages);
  for (let i = lastUser + 1; i < messages.length; i++) {
    const m = messages[i]!;
    if (m.role === "assistant" && (m.content?.trim() || m.thought?.trim())) {
      return false;
    }
  }
  return true;
}

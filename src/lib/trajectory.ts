/** 轨迹台账纯数据模型：把会话消息投影为可逐条检视的记录流水。 */

import type { AcpSubagentInfo } from "./acp/store";
import {
  messageSegments,
  parseCompactContent,
  toolCallIdOf,
  type ChatMessage,
  type ContextCompactMeta,
  type MessageAttachment,
  type MessageToolSegment,
} from "./session";
import type { TurnLatencySummary } from "./turnLatency";

export type TrajectoryRecordKind =
  | "user"
  | "assistant"
  | "thinking"
  | "tool"
  | "subagent"
  | "compacted"
  | "error"
  | "cancelled";

export type TrajectoryRecordStatus = "completed" | "running" | "failed";

export interface TrajectoryRecord {
  /** 稳定 React key 与去重标识。 */
  key: string;
  kind: TrajectoryRecordKind;
  /** 1-based 台账序号（#N），构建完成后统一分配。 */
  index: number;
  /** 所属轮次；首条用户消息之前的前缀记录为 0。 */
  turn: number;
  /** 是否为本轮第一条记录（决定 Turn N 标签与左轨道起点）。 */
  opensTurn: boolean;
  /** 单行预览标题。 */
  title: string;
  status: TrajectoryRecordStatus;
  createdAt?: string;
  durationMs?: number | null;
  /** 工具输入 / 用户正文。 */
  input?: string;
  /** 工具输出 / 回复正文 / 错误正文。 */
  output?: string;
  /** 思考正文。 */
  thinking?: string;
  /** 回复正文记录携带的本轮观测。 */
  metrics?: TurnLatencySummary;
  compactMeta?: ContextCompactMeta;
  toolKind?: string;
  path?: string;
  attachments?: MessageAttachment[];
  subagent?: AcpSubagentInfo;
}

export interface TrajectoryStats {
  total: number;
  tools: number;
  failed: number;
  running: number;
  turns: number;
  /** 已知记录的耗时合计（毫秒）。 */
  totalDurationMs: number;
  inputTokens: number | null;
  cacheReadTokens: number | null;
  cacheCreationTokens: number | null;
}

const TITLE_LIMIT = 120;
export const TRAJECTORY_DETAIL_LIMIT = 4_000;

/** 折叠空白并截断为单行预览。 */
export function trajectorySingleLine(
  text: string | null | undefined,
  limit = TITLE_LIMIT,
): string {
  const flat = (text || "").replace(/\s+/g, " ").trim();
  if (flat.length <= limit) return flat;
  return `${flat.slice(0, limit)}…`;
}

/** 详情区避免把超大文本完整挂进 DOM。 */
export function compactTrajectoryDetail(
  value: string | null | undefined,
  limit = TRAJECTORY_DETAIL_LIMIT,
): string {
  if (!value) return "";
  return value.length > limit ? `${value.slice(0, limit)}\n…` : value;
}

/** 工具段 / 工具行的状态映射。 */
export function toolRecordStatus(
  tool: Pick<MessageToolSegment, "status" | "isError" | "streaming">,
): TrajectoryRecordStatus {
  if (tool.isError || tool.status === "failed" || tool.status === "error") {
    return "failed";
  }
  if (
    tool.streaming ||
    tool.status === "in_progress" ||
    tool.status === "pending" ||
    tool.status === "running" ||
    tool.status === ""
  ) {
    return "running";
  }
  return "completed";
}

function toolRecord(
  source: MessageToolSegment,
  extra: { key: string; turn: number },
): TrajectoryRecord {
  return {
    key: extra.key,
    kind: "tool",
    index: 0,
    turn: extra.turn,
    opensTurn: false,
    title:
      trajectorySingleLine(source.title) ||
      source.toolKind ||
      `#${source.toolCallId.slice(0, 8)}`,
    status: toolRecordStatus(source),
    durationMs: source.durationMs ?? null,
    input: source.input,
    output: source.output ?? source.detail ?? undefined,
    toolKind: source.toolKind,
    path: source.path,
  };
}

/** 把会话消息与子代理投影为轨迹台账记录（按到达顺序）。 */
export function buildTrajectoryRecords(
  messages: readonly ChatMessage[],
  subagents: readonly AcpSubagentInfo[] = [],
): TrajectoryRecord[] {
  const records: TrajectoryRecord[] = [];
  const seenToolCallIds = new Set<string>();
  let turn = 0;

  for (const message of messages) {
    if (message.role === "user") {
      turn += 1;
      records.push({
        key: `${message.id}:user`,
        kind: "user",
        index: 0,
        turn,
        opensTurn: true,
        title: trajectorySingleLine(message.content),
        status: "completed",
        createdAt: message.createdAt,
        input: message.content,
        attachments: message.attachments,
      });
      continue;
    }

    if (
      message.marker === "context_compact" ||
      message.compactMeta ||
      message.content.startsWith("context_compact")
    ) {
      const meta =
        message.compactMeta ?? parseCompactContent(message.content) ?? undefined;
      const tokens =
        meta?.tokensBefore != null && meta?.tokensAfter != null
          ? ` ${meta.tokensBefore}→${meta.tokensAfter}`
          : "";
      records.push({
        key: `${message.id}:compacted`,
        kind: "compacted",
        index: 0,
        turn,
        opensTurn: false,
        title: `${meta?.trigger ?? "auto"}${tokens}`.trim(),
        status: "completed",
        createdAt: message.createdAt,
        compactMeta: meta,
      });
      continue;
    }

    if (message.marker === "turn_cancelled") {
      records.push({
        key: `${message.id}:cancelled`,
        kind: "cancelled",
        index: 0,
        turn,
        opensTurn: false,
        title: trajectorySingleLine(message.content) || "turn_cancelled",
        status: "completed",
        createdAt: message.createdAt,
        output: message.content,
      });
      continue;
    }

    if (message.role === "assistant") {
      if (message.isError) {
        records.push({
          key: `${message.id}:error`,
          kind: "error",
          index: 0,
          turn,
          opensTurn: false,
          title: trajectorySingleLine(message.content),
          status: "failed",
          createdAt: message.createdAt,
          output: message.content,
        });
        continue;
      }

      const segments = messageSegments(message);
      let metricsAttached = false;
      let thinkingAttached = false;
      for (const [si, segment] of segments.entries()) {
        if (segment.kind === "thought") {
          records.push({
            key: `${message.id}:thought:${si}`,
            kind: "thinking",
            index: 0,
            turn,
            opensTurn: false,
            title: trajectorySingleLine(segment.text),
            status: message.streaming ? "running" : "completed",
            createdAt: message.createdAt,
            thinking: segment.text,
            durationMs: thinkingAttached
              ? null
              : message.thinkingDurationMs ?? null,
          });
          thinkingAttached = true;
          continue;
        }
        if (segment.kind === "content") {
          records.push({
            key: `${message.id}:content:${si}`,
            kind: "assistant",
            index: 0,
            turn,
            opensTurn: false,
            title: trajectorySingleLine(segment.text),
            status: message.streaming ? "running" : "completed",
            createdAt: message.createdAt,
            output: segment.text,
            metrics: metricsAttached ? undefined : message.turnMetrics,
          });
          metricsAttached = true;
          continue;
        }
        seenToolCallIds.add(segment.toolCallId);
        records.push(
          toolRecord(segment, { key: `${message.id}:tool:${si}`, turn }),
        );
      }
      continue;
    }

    // 工具行：完整 journal 行带 tool_step 标记；重放映射可能只留下
    // {role:"tool", content:输出}——peri 把无 tool_use 块的工具调用全存为
    // 独立 tool 行，两类都必须投影为工具记录。
    if (message.role === "tool") {
      const toolCallId = toolCallIdOf(message);
      if (seenToolCallIds.has(toolCallId)) continue;
      seenToolCallIds.add(toolCallId);
      const status = (message.toolStatus || "completed").toLowerCase();
      records.push({
        key: `tool:${toolCallId}`,
        kind: "tool",
        index: 0,
        turn,
        opensTurn: false,
        title:
          trajectorySingleLine(message.content) ||
          message.toolKind ||
          `#${toolCallId.slice(0, 8)}`,
        status: toolRecordStatus({
          status,
          isError: !!message.isError,
          streaming: !!message.streaming,
        }),
        createdAt: message.createdAt,
        output: message.toolDetail || message.content,
        toolKind: message.toolKind,
        path: message.toolPath,
      });
    }
  }

  for (const [ai, agent] of subagents.entries()) {
    records.push({
      key: `subagent:${agent.agent_id || ai}:${agent.started_at}`,
      kind: "subagent",
      index: 0,
      turn,
      opensTurn: false,
      title: agent.agent_name || `#${ai + 1}`,
      status:
        agent.status === "running"
          ? "running"
          : agent.status === "failed"
            ? "failed"
            : "completed",
      createdAt: new Date(agent.started_at).toISOString(),
      durationMs:
        agent.stopped_at != null
          ? Math.max(0, agent.stopped_at - agent.started_at)
          : null,
      output: agent.result ?? undefined,
      subagent: agent,
    });
  }

  return records.map((record, index) => ({ ...record, index: index + 1 }));
}

/** 多词 AND、大小写不敏感的台账过滤。 */
export function filterTrajectoryRecords(
  records: readonly TrajectoryRecord[],
  query: string,
): TrajectoryRecord[] {
  const terms = query
    .trim()
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean);
  if (!terms.length) return records.slice();
  return records.filter((record) => {
    const haystack = [
      record.title,
      record.input,
      record.output,
      record.thinking,
      record.toolKind,
      record.path,
      record.subagent?.agent_name,
      record.subagent?.result,
      record.compactMeta?.summaryPreview,
    ]
      .filter((value): value is string => typeof value === "string")
      .join("\n")
      .toLowerCase();
    return terms.every((term) => haystack.includes(term));
  });
}

function sumTokens(
  records: readonly TrajectoryRecord[],
  field: "inputTokens" | "cacheReadTokens" | "cacheCreationTokens",
): number | null {
  let total: number | null = null;
  for (const record of records) {
    const value = record.metrics?.[field];
    if (typeof value !== "number" || !Number.isFinite(value)) continue;
    total = (total ?? 0) + value;
  }
  return total;
}

/** 台账统计：总数、工具数、失败数、轮数与 Token 汇总。 */
export function summarizeTrajectory(
  records: readonly TrajectoryRecord[],
): TrajectoryStats {
  let tools = 0;
  let failed = 0;
  let running = 0;
  let turns = 0;
  let totalDurationMs = 0;
  for (const record of records) {
    if (record.kind === "tool") tools += 1;
    if (record.status === "failed") failed += 1;
    if (record.status === "running") running += 1;
    if (record.turn > turns) turns = record.turn;
    if (typeof record.durationMs === "number" && record.durationMs > 0) {
      totalDurationMs += record.durationMs;
    }
  }
  return {
    total: records.length,
    tools,
    failed,
    running,
    turns,
    totalDurationMs,
    inputTokens: sumTokens(records, "inputTokens"),
    cacheReadTokens: sumTokens(records, "cacheReadTokens"),
    cacheCreationTokens: sumTokens(records, "cacheCreationTokens"),
  };
}

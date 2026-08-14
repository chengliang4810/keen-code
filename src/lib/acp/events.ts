/** ACP 原生事件契约（桌面端）。

事件到达路径：Tauri 后端把 ACP 通知转发为 `acp://*` 事件，
载荷统一为 `{ method, params }` JSON-RPC 通知信封。本模块负责解析。
 */

import type {
  AcpCompactTrigger,
  AcpSystemNotificationLevel,
  AcpSystemNotificationWireLevel,
} from "./types";

/** acp://session-update 载荷（method: "session/update"）。 */
export interface SessionUpdateEnvelope {
  method: "session/update";
  params: {
    sessionId: string;
    update: SessionUpdate;
    /** 后端附加的来源信息；sourceAgentId 存在时更新应归入对应子 Agent 时间线。 */
    _peri?: { sourceAgentId?: string };
  };
}

/** ACP SessionUpdate —— serde tag = "sessionUpdate"（snake_case）。 */
export type SessionUpdate =
  | UserMessageChunkUpdate
  | AgentMessageChunkUpdate
  | AgentThoughtChunkUpdate
  | ToolCallUpdateEvent
  | ToolCallUpdateEventUpdate
  | PlanUpdate
  | UsageUpdateEvent
  | AvailableCommandsUpdate
  | CurrentModeUpdate
  | ConfigOptionUpdate
  | SessionInfoUpdate;

/** ACP 内容块；文本位于 ContentChunk.content 中。 */
export type AcpContentBlock =
  | { type: "text"; text: string; _meta?: Record<string, unknown> }
  | {
      type: "image";
      data: string;
      mimeType: string;
      _meta?: Record<string, unknown>;
    }
  | {
      type: "audio";
      data: string;
      mimeType: string;
      _meta?: Record<string, unknown>;
    }
  | {
      type: "resource_link";
      name: string;
      uri: string;
      _meta?: Record<string, unknown>;
    }
  | {
      type: "resource";
      resource: Record<string, unknown>;
      _meta?: Record<string, unknown>;
    };

/** ACP 当前工具类别。 */
export type AcpToolKind =
  | "read"
  | "edit"
  | "delete"
  | "move"
  | "search"
  | "execute"
  | "think"
  | "fetch"
  | "switch_mode"
  | "other";

/** ACP 当前工具状态。 */
export type AcpToolStatus =
  | "pending"
  | "in_progress"
  | "completed"
  | "failed";

/** ACP 流式内容包装，字段形状来自 agent-client-protocol SDK。 */
export interface ContentChunk {
  content: AcpContentBlock;
  messageId?: string | null;
  _meta?: Record<string, unknown>;
}

export interface UserMessageChunkUpdate extends ContentChunk {
  sessionUpdate: "user_message_chunk";
}

export interface AgentMessageChunkUpdate extends ContentChunk {
  sessionUpdate: "agent_message_chunk";
}

export interface AgentThoughtChunkUpdate extends ContentChunk {
  sessionUpdate: "agent_thought_chunk";
}

/** tool_call：新工具调用开始。 */
export interface ToolCallUpdateEvent {
  sessionUpdate: "tool_call";
  toolCallId: string;
  title: string;
  kind?: AcpToolKind;
  status?: AcpToolStatus;
  rawInput?: unknown;
  _meta?: Record<string, unknown>;
}

/** tool_call_update：工具状态/结果变化。 */
export interface ToolCallUpdateEventUpdate {
  sessionUpdate: "tool_call_update";
  toolCallId: string;
  kind?: AcpToolKind;
  status?: AcpToolStatus;
  title?: string;
  rawOutput?: unknown;
  _meta?: Record<string, unknown>;
}

/** plan：Todo 数据源（entries 无 id，客户端按序管理）。 */
export interface PlanUpdate {
  sessionUpdate: "plan";
  entries: PlanEntry[];
  _meta?: Record<string, unknown>;
}

export interface PlanEntry {
  content: string;
  priority: "high" | "medium" | "low";
  status: "pending" | "in_progress" | "completed";
  _meta?: Record<string, unknown>;
}

export interface UsageUpdateEvent {
  sessionUpdate: "usage_update";
  used: number;
  size: number;
  _meta?: Record<string, unknown>;
}

export interface AvailableCommandsUpdate {
  sessionUpdate: "available_commands_update";
  availableCommands: Array<{
    name: string;
    description: string;
    input?: Record<string, unknown> | null;
    _meta?: Record<string, unknown>;
  }>;
  _meta?: Record<string, unknown>;
}

export interface CurrentModeUpdate {
  sessionUpdate: "current_mode_update";
  currentModeId: string;
  _meta?: Record<string, unknown>;
}

export interface ConfigOptionUpdate {
  sessionUpdate: "config_option_update";
  configOptions: Array<Record<string, unknown>>;
  _meta?: Record<string, unknown>;
}

export interface SessionInfoUpdate {
  sessionUpdate: "session_info_update";
  title?: string | null;
  updatedAt?: string | null;
  _meta?: Record<string, unknown>;
}

/** acp://agent-event 载荷（method: "peri/agent_event"）—— event_json 是序列化的 AcpEvent。 */
export interface AgentEventEnvelope {
  method: "peri/agent_event";
  params: {
    sessionId: string;
    event_json: string;
  };
}

/** AcpEvent —— serde tag="type", content="value"，snake_case。 */
export type AcpEvent =
  | { type: "state_snapshot"; value: { messages_json: string } }
  | { type: "turn_committed"; value: { messages_json: string; steps: number } }
  | { type: "turn_suspended"; value: { turn_id: string; agent_id: string } }
  | {
      type: "state_snapshot_meta";
      value: {
        message_count: number;
        total_tokens: number;
        current_step: number;
        consecutive_failures: number;
        budget_pct: number | null;
        context_total_tokens: number | null;
      };
    }
  | {
      type: "subagent_started";
      value: { agent_name: string; instance_id: string; is_background: boolean };
    }
  | {
      type: "subagent_stopped";
      value: {
        agent_name: string;
        result: string;
        is_error: boolean;
        instance_id: string;
      };
    }
  | { type: "compact_started" }
  | {
      type: "compact_completed";
      value: {
        summary: string;
        files: Array<{ path: string; lines: number }>;
        skills: string[];
        micro_cleared: number;
        messages_json: string;
        strategy: string;
        trigger: AcpCompactTrigger;
        outcome: string;
      };
    }
  | { type: "compact_error"; value: { message: string } }
  | {
      type: "background_task_completed";
      value: {
        task_id: string;
        agent_name: string;
        success: boolean;
        output: string;
        tool_calls_count: number;
        duration_ms: number;
        child_thread_id: string | null;
      };
    }
  | { type: "bg_tool_step"; value: { child_thread_id: string } }
  | { type: "agent_execution_failed"; value: { message: string } }
  | {
      type: "context_warning";
      value: { used_tokens: number; total_tokens: number; percentage: number };
    }
  | {
      type: "system_notification";
      value: { text: string; level: AcpSystemNotificationLevel };
    }
  | { type: "oauth_needed"; value: { server_name: string; auth_url: string } }
  | { type: "oauth_completed"; value: { server_name: string } }
  | { type: "oauth_failed"; value: { server_name: string; error: string } }
  | { type: "oauth_restored"; value: { server_name: string } }
  | { type: "llm_retrying"; value: { attempt: number; max_attempts: number; delay_ms: number; error: string } }
  | {
      type: "goal_changed";
      value: {
        revision: number;
        change: "created" | "updated" | "transitioned";
        goal: GoalRecordDto;
      };
    };

/** Goal wire 形状，与 Rust GoalRecordDto 字段精确同名。 */
export interface GoalRecordDto {
  id: string;
  title: string;
  scope: "project";
  status: "active" | "completed" | "blocked";
  description?: string | null;
  progress_percent?: number | null;
  created_at: string;
  updated_at: string;
  objective: string;
  token_budget?: number | null;
  tokens_used: number;
  time_used_seconds: number;
  blocked_reason?: string | null;
}

/** acp://recovery-status 载荷（session/recovery 通知，snake_case）。 */
export interface RecoveryEnvelope {
  method: "session/recovery";
  params: {
    session_id: string;
    status: "not_required" | "restoring";
    cursor?: { epoch: string; sequence: number } | null;
    pending_tools: PendingToolItem[];
    reason?: string | null;
  };
}

export interface PendingToolItem {
  call_id: string;
  name: string;
  status: "unknown_outcome";
  started_at_unix_ms: number;
  detail?: string | null;
}

/** acp://elicitation 载荷。 */
export interface ElicitationEnvelope {
  method: "elicitation/create";
  rpcId: number;
  params: {
    mode: "form";
    sessionId: string;
    message?: string;
    requestedSchema: {
      type: string;
      properties?: Record<string, unknown>;
    };
  };
}

/** acp://agent-done 载荷。 */
export interface AgentDoneEnvelope {
  method: "peri/agent_event_done";
  params: {
    sessionId: string;
    stopReason: string;
  };
}

/** 解析当前 peri/agent_event 契约；未知事件标签不会进入当前投影。 */
export function parseAgentEvent(eventJson: string): AcpEvent | null {
  try {
    const event = JSON.parse(eventJson) as {
      type?: unknown;
      value?: Record<string, unknown>;
    };
    if (!event || typeof event !== "object" || typeof event.type !== "string") {
      return null;
    }
    switch (event.type) {
      case "state_snapshot":
      case "turn_committed":
      case "turn_suspended":
      case "state_snapshot_meta":
      case "subagent_started":
      case "subagent_stopped":
      case "compact_started":
      case "compact_error":
      case "background_task_completed":
      case "bg_tool_step":
      case "agent_execution_failed":
      case "context_warning":
      case "llm_retrying":
      case "goal_changed":
        return event as AcpEvent;
      case "compact_completed": {
        const trigger = event.value?.trigger === "manual" ? "manual" : "auto";
        return {
          ...event,
          value: { ...event.value, trigger },
        } as AcpEvent;
      }
      case "system_notification": {
        const level = normalizeSystemNotificationLevel(event.value?.level);
        return {
          ...event,
          value: { ...event.value, level },
        } as AcpEvent;
      }
      case "oauth_needed":
      case "oauth_completed":
      case "oauth_failed":
      case "oauth_restored":
        return event as AcpEvent;
      default:
        return null;
    }
  } catch {
    return null;
  }
}

/** 将 Peri 的 warn/warning 双写法归一化为前端唯一等级。 */
export function normalizeSystemNotificationLevel(
  level: unknown,
): AcpSystemNotificationLevel {
  const wireLevel = level as AcpSystemNotificationWireLevel;
  if (wireLevel === "warn" || wireLevel === "warning") return "warning";
  if (wireLevel === "error") return "error";
  return "info";
}

/** 判断实时更新是否应驱动主 Agent 的 streaming 状态。 */
export function shouldDriveMainSessionStreaming(
  update: SessionUpdate,
  sourceAgentId?: string,
): boolean {
  if (sourceAgentId) return false;
  switch (update.sessionUpdate) {
    case "user_message_chunk":
    case "agent_message_chunk":
    case "agent_thought_chunk":
    case "tool_call":
    case "tool_call_update":
    case "plan":
      return true;
    default:
      return false;
  }
}

/** 判断 session/update 是否带 periReplay 标记（历史重放 vs 实时）。 */
export function isReplayedUpdate(update: SessionUpdate): boolean {
  const meta = (update as { _meta?: Record<string, unknown> })._meta;
  return Boolean(meta?.periReplay);
}

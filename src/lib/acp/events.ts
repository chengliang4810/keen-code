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
    /** 当前前台请求的稳定标识；历史重放与会话级更新可省略。 */
    requestId?: string;
    update: SessionUpdate;
    /**
     * 后端附加的来源信息；Peri v2 的主 Agent 与子 Agent 都可能携带
     * sourceAgentId，消费端需先与已登记的子 Agent 身份匹配再决定时间线。
     */
    _peri?: {
      /** 发出该更新的 Agent 身份。 */
      sourceAgentId?: string;
    };
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
  content?: Array<{
    type: string;
    content?: AcpContentBlock;
  }>;
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
    /** 前台请求事件必须携带；项目级/后台生命周期事件可省略。 */
    requestId?: string;
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
      value: {
        agent_name: string;
        agent_nickname: import("@/lib/agentNicknames").AgentNicknameRef;
        instance_id: string;
        is_background: boolean;
      };
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
  | {
      type: "agent_execution_failed";
      value: { code: string; message: string };
    }
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

/** acp://agent-done 载荷；只有带 requestId 与完成时间的前台请求可收口。 */
export type AgentDoneEnvelope = {
  method: "peri/agent_event_done";
  params:
    | {
        sessionId: string;
        requestId: string;
        stopReason: string;
        /** KeenCode Host 附加的唯一完成观测。 */
        _keencode: {
          /** Host 观测到本轮完成通知的 Epoch 毫秒。 */
          completedAtMs: number;
        };
      }
    | {
        sessionId: string;
        stopReason: string;
        requestId?: undefined;
        _keencode?: undefined;
      };
};

export type ForegroundRequestDoneParams = Extract<
  AgentDoneEnvelope["params"],
  { _keencode: { completedAtMs: number } }
>;

/** 后台任务完成不得收口或改写当前前台请求。 */
export function isForegroundRequestDone(
  params: AgentDoneEnvelope["params"],
): params is ForegroundRequestDoneParams {
  return Boolean(
    params.requestId &&
      params._keencode &&
      Number.isFinite(params._keencode.completedAtMs),
  );
}

/** peri/unstable_event 的透明扩展信封。 */
export interface UnstableEventEnvelope {
  method: "peri/unstable-event";
  params: {
    sessionId: string;
    /** 与前台 LLM 事件关联的唯一请求标识。 */
    requestId?: string;
    event: string;
    data?: {
      /** Provider 首帧在运行时被解析的 Epoch 毫秒。 */
      at_ms?: number;
      /** 当前 LLM 消息标识，仅用于诊断关联。 */
      message_id?: string;
      /** 子 Agent 来源；主 Agent 事件缺省。 */
      source_agent_id?: string;
      [key: string]: unknown;
    } | null;
  };
}

/** 只有携带当前 requestId 的终态通知才能结束对应的活跃回合。 */
export function shouldAcceptAgentDone(
  activeRequestId: string | null | undefined,
  doneRequestId: string | null | undefined,
): boolean {
  return Boolean(
    activeRequestId && doneRequestId && activeRequestId === doneRequestId,
  );
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

/** 合并同一时间线连续到达的文本分片，避免每个 token 都扩容实时字符串。 */
export function mergeSessionTextUpdates(
  current: SessionUpdate | undefined,
  next: SessionUpdate,
): AgentMessageChunkUpdate | AgentThoughtChunkUpdate | null {
  if (
    next.sessionUpdate !== "agent_message_chunk" &&
    next.sessionUpdate !== "agent_thought_chunk"
  ) {
    return null;
  }
  if (next.content.type !== "text") return null;
  if (!current) return next;
  if (
    current.sessionUpdate !== next.sessionUpdate ||
    current.content.type !== "text"
  ) {
    return null;
  }
  return {
    ...next,
    content: {
      ...next.content,
      text: current.content.text + next.content.text,
    },
  };
}

/**
 * 判断一条 session/update 是否可以写入当前前台投影。
 *
 * 历史重放、子 Agent 与会话级配置不属于当前前台请求；其余实时内容、
 * 工具、计划和 usage 必须与当前 active requestId 精确匹配。缺失关联字段时
 * fail closed，避免旧 IPC 消息污染同一 Session 的下一轮。
 */
export function shouldApplySessionUpdate(
  params: SessionUpdateEnvelope["params"],
  activeRequestId: string | null | undefined,
  sourceAgentId?: string,
): boolean {
  if (!isRequestScopedSessionUpdate(params, sourceAgentId)) return true;
  return Boolean(activeRequestId && params.requestId === activeRequestId);
}

/** 历史、已确认子 Agent 与会话配置以外的更新都属于前台请求。 */
export function isRequestScopedSessionUpdate(
  params: SessionUpdateEnvelope["params"],
  sourceAgentId?: string,
): boolean {
  if (isReplayedUpdate(params.update)) return false;
  if (sourceAgentId) return false;
  switch (params.update.sessionUpdate) {
    case "available_commands_update":
    case "current_mode_update":
    case "config_option_update":
    case "session_info_update":
      return false;
    default:
      return true;
  }
}

/**
 * 项目级 Goal 与独立后台 Agent 生命周期可跨前台请求；其余 agent-event
 * 必须属于当前 active request，尤其不能让迟到的旧失败覆盖新回合。
 */
export function shouldApplyAgentEvent(
  params: AgentEventEnvelope["params"],
  event: AcpEvent,
  activeRequestId: string | null | undefined,
): boolean {
  if (!isRequestScopedAgentEvent(event)) return true;
  return Boolean(activeRequestId && params.requestId === activeRequestId);
}

/** 项目级 Goal 与后台 Agent 生命周期之外的 agent-event 属于前台请求。 */
export function isRequestScopedAgentEvent(event: AcpEvent): boolean {
  switch (event.type) {
    case "goal_changed":
    case "subagent_started":
    case "subagent_stopped":
    case "background_task_completed":
    case "bg_tool_step":
      return false;
    default:
      return true;
  }
}

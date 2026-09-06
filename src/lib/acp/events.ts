/** ACP 标准更新与 KeenCode 生命周期事件的唯一前端线格式。 */

import type { AcpSystemNotificationLevel } from "./types";
import { isStandardResourceLink, parseFileChangeResourceLink } from "./fileChanges";

/** 当前两类投递信封共同使用的 Schema 版本。 */
export const ACP_DELIVERY_SCHEMA_VERSION = 1 as const;

/** KeenCode 事件标识允许的最大 UTF-8 字节数。 */
const MAX_EVENT_IDENTIFIER_BYTES = 256;
/** Agent 路径允许的最大 UTF-8 字节数。 */
const MAX_AGENT_PATH_BYTES = 1024;
/** Agent 委派任务正文允许的最大 UTF-8 字节数。 */
const MAX_AGENT_TASK_BYTES = 256 * 1024;
/** 用户可见事件说明和脱敏摘要允许的最大 UTF-8 字节数。 */
const MAX_EVENT_MESSAGE_BYTES = 4096;
/** MCP OAuth 授权地址允许的最大 UTF-8 字节数。 */
const MAX_OAUTH_AUTHORIZATION_URL_BYTES = 4096;
/** MCP OAuth 项目路径允许的最大 UTF-8 字节数；与 Rust ACP 事件边界一致。 */
const MAX_OAUTH_PROJECT_PATH_BYTES = 4 * 1024;
/** 单次模型请求允许上报的最大重试次数。 */
const MAX_MODEL_RETRY_ATTEMPTS = 32;
/** 单次模型重试允许等待的最大毫秒数。 */
const MAX_MODEL_RETRY_DELAY_MS = 10 * 60 * 1000;
/** 单个后台任务允许记录的最大持续时间，当前为三十天。 */
const MAX_BACKGROUND_TASK_DURATION_MS = 30 * 24 * 60 * 60 * 1000;

/** ACP SessionUpdate 的封闭联合。 */
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

/** ACP 内容块；标准 `_meta` 透传，KeenCode 只读取自有 `keencode/*` 命名空间。 */
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
      /** ACP 标准资源描述，可由服务端提供。 */
      description?: string | null;
      /** ACP 标准资源媒体类型。 */
      mimeType?: string | null;
      /** 完整资源大小，不是本次内联传输大小。 */
      size?: number | null;
      /** 资源的展示标题。 */
      title?: string | null;
      /** ACP 标准资源注释，不作为工具或 Session 身份依据。 */
      annotations?: Record<string, unknown> | null;
      _meta?: Record<string, unknown>;
    }
  | {
      type: "resource";
      resource: Record<string, unknown>;
      _meta?: Record<string, unknown>;
    };

/** ACP 工具类别。 */
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

/** ACP 工具执行状态。 */
export type AcpToolStatus =
  | "pending"
  | "in_progress"
  | "completed"
  | "failed";

/** 自研 Runtime 的精确工具终态；不扩展标准 ACP 状态枚举。 */
export type ToolCompletionStatus = "succeeded" | "failed" | "cancelled" | "side_effect_unknown";

/** 只读取与标准状态一致的自有终态，外部 ACP 不需要提供该元数据。 */
export function toolCompletionStatusOf(update: { status?: unknown; _meta?: Record<string, unknown> }): ToolCompletionStatus | undefined {
  const outcome = update._meta?.["keencode/toolOutcome"];
  if (outcome === "succeeded" && update.status === "completed") return outcome;
  if ((outcome === "failed" || outcome === "cancelled" || outcome === "side_effect_unknown") && update.status === "failed") return outcome;
  return undefined;
}

/** 已出现的自有元数据必须合法，不能静默丢失取消或副作用未知信息。 */
function hasValidToolOutcome(value: Record<string, unknown>): boolean {
  if (!hasValidMeta(value)) return false;
  const meta = value._meta as Record<string, unknown> | undefined;
  return !meta || !Object.hasOwn(meta, "keencode/toolOutcome") ||
    toolCompletionStatusOf({ status: value.status, _meta: meta }) !== undefined;
}

/** ACP 流式内容的公共字段。 */
export interface ContentChunk {
  /** 当前分片内容。 */
  content: AcpContentBlock;
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** 用户消息分片。 */
export interface UserMessageChunkUpdate extends ContentChunk {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "user_message_chunk";
}

/** Agent 正文分片。 */
export interface AgentMessageChunkUpdate extends ContentChunk {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "agent_message_chunk";
}

/** Agent 思考分片。 */
export interface AgentThoughtChunkUpdate extends ContentChunk {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "agent_thought_chunk";
}

/** 标准 ACP 工具内容；文件差异必须是顶层 Diff，而非自定义 rawOutput。 */
export type AcpToolCallContent =
  | {
      /** 普通工具内容包装项。 */
      type: "content";
      /** ACP 标准内容块。 */
      content: AcpContentBlock;
      /** 标准透传元数据。 */
      _meta?: Record<string, unknown>;
    }
  | {
      /** 标准文件变更。 */
      type: "diff";
      /** 变更文件路径。 */
      path: string;
      /** 原始正文；省略或 null 表示文件原先不存在。 */
      oldText?: string | null;
      /** 修改后正文，允许空文件。 */
      newText: string;
      /** 标准透传元数据。 */
      _meta?: Record<string, unknown>;
    }
  | {
      /** 标准终端引用。 */
      type: "terminal";
      /** 已创建终端的稳定标识。 */
      terminalId: string;
      /** 标准透传元数据。 */
      _meta?: Record<string, unknown>;
    };

/** 新工具调用开始。 */
export interface ToolCallUpdateEvent {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "tool_call";
  /** 工具调用稳定标识。 */
  toolCallId: string;
  /** 用户可见标题。 */
  title: string;
  /** ACP 工具类别。 */
  kind?: AcpToolKind;
  /** 当前执行状态。 */
  status?: AcpToolStatus;
  /** 结构化原始输入。 */
  rawInput?: unknown;
  /** 标准工具内容，可包含精确文件快照。 */
  content?: AcpToolCallContent[];
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** 已有工具调用的状态或结果更新。 */
export interface ToolCallUpdateEventUpdate {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "tool_call_update";
  /** 工具调用稳定标识。 */
  toolCallId: string;
  /** ACP 工具类别。 */
  kind?: AcpToolKind;
  /** 当前执行状态。 */
  status?: AcpToolStatus;
  /** 用户可见标题。 */
  title?: string;
  /** 标准工具结果内容。 */
  content?: AcpToolCallContent[];
  /** 结构化原始输出。 */
  rawOutput?: unknown;
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** 当前 Session 的 Todo 计划。 */
export interface PlanUpdate {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "plan";
  /** 有序计划项。 */
  entries: PlanEntry[];
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** 单个 ACP 计划项。 */
export interface PlanEntry {
  /** 计划正文。 */
  content: string;
  /** 计划优先级。 */
  priority: "high" | "medium" | "low";
  /** 计划执行状态。 */
  status: "pending" | "in_progress" | "completed";
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** ACP 标准上下文占用更新。 */
export interface UsageUpdateEvent {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "usage_update";
  /** 当前上下文已使用 Token。 */
  used: number;
  /** 模型上下文窗口 Token 上限。 */
  size: number;
  /** 可选累计成本。 */
  cost?: { amount: number; currency: string };
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** Session 可用命令列表更新。 */
export interface AvailableCommandsUpdate {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "available_commands_update";
  /** 当前完整命令列表。 */
  availableCommands: Array<{
    /** 命令名称。 */
    name: string;
    /** 命令说明。 */
    description: string;
    /** 可选输入 Schema。 */
    input?: Record<string, unknown> | null;
    /** ACP 标准透传元数据。 */
    _meta?: Record<string, unknown>;
  }>;
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** 当前 Session 模式更新。 */
export interface CurrentModeUpdate {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "current_mode_update";
  /** 当前模式稳定标识。 */
  currentModeId: string;
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** 当前 Session 配置选项更新。 */
export interface ConfigOptionUpdate {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "config_option_update";
  /** 当前完整配置选项。 */
  configOptions: Array<Record<string, unknown>>;
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** 当前 Session 基本信息更新。 */
export interface SessionInfoUpdate {
  /** SessionUpdate 判别字段。 */
  sessionUpdate: "session_info_update";
  /** 可选用户可见标题。 */
  title?: string | null;
  /** 可选最近更新时间。 */
  updatedAt?: string | null;
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** 标准 SessionUpdate 的严格投递信封。 */
export interface SessionUpdateDeliveryEnvelope {
  /** 信封 Schema 版本，当前固定为 1。 */
  schemaVersion: typeof ACP_DELIVERY_SCHEMA_VERSION;
  /** 根 Session 稳定标识。 */
  sessionId: string;
  /** Turn 级更新的稳定 Turn 标识。 */
  turnId?: string;
  /** Turn 级更新的来源 Agent 标识。 */
  sourceAgentId?: string;
  /** 当前 Runtime 内按 Session 单调递增的共享投递序号。 */
  deliverySequence: number;
  /** 更新发生时的 UTC Unix 毫秒时间。 */
  occurredAtMs: number;
  /** 原样保留的 ACP 标准更新。 */
  update: SessionUpdate;
}

/** Turn 失败的稳定安全分类。 */
export type TurnFailureKind =
  | "model"
  | "context"
  | "tool"
  | "storage"
  | "internal";

/** 上下文压缩失败的稳定分类。 */
export type CompactionFailureKind =
  | "model"
  | "budget"
  | "storage"
  | "invalid_result";

/** 单层 Agent 的生命周期状态。 */
export type AgentLifecycleStatus =
  | "pending"
  | "running"
  | "waiting"
  | "completed"
  | "interrupted"
  | "failed"
  | "stopped";

/** Session 恢复状态。 */
export type RecoveryState = "pending" | "replaying" | "ready" | "failed";

/** Session 级后台任务类别。 */
export type BackgroundTaskKind = "shell" | "agent";

/** Session 级后台任务终态。 */
export type BackgroundTaskTerminalStatus =
  | "succeeded"
  | "failed"
  | "cancelled";

/** MCP OAuth Host 通知；事件始终带有项目作用域，不绑定任何 Session。 */
export type McpOAuthEvent =
  | {
      /** 等待用户在浏览器中完成授权。 */
      type: "mcp_oauth_authorization_required";
      /** 发起授权的项目路径。 */
      projectPath: string;
      /** MCP Server 稳定名称。 */
      serverName: string;
      /** Agent Runtime 生成的授权地址。 */
      authorizationUrl: string;
    }
  | {
      /** MCP Server 已完成授权。 */
      type: "mcp_oauth_authorized";
      /** 完成授权的项目路径。 */
      projectPath: string;
      /** MCP Server 稳定名称。 */
      serverName: string;
    }
  | {
      /** MCP OAuth 授权或刷新失败。 */
      type: "mcp_oauth_failed";
      /** 发生失败的项目路径。 */
      projectPath: string;
      /** MCP Server 稳定名称。 */
      serverName: string;
      /** 已脱敏的用户可见失败摘要。 */
      message: string;
    };

/** 标准 ACP 无法表达的 KeenCode 类型化事件。 */
export type KeenCodeEvent =
  | { type: "turn_started"; rootTurnId: string; parentTurnId?: string }
  | { type: "turn_completed" }
  | { type: "turn_cancelled" }
  | { type: "turn_failed"; failureKind: TurnFailureKind; message: string }
  | {
      type: "agent_spawned";
      agentId: string;
      parentAgentId: string;
      agentPath: string;
      task: string;
      parentTurnId: string;
      rootTurnId: string;
    }
  | { type: "agent_status_changed"; agentId: string; status: AgentLifecycleStatus }
  | {
      type: "agent_message_queued";
      messageId: string;
      fromAgentId: string;
      toAgentId: string;
    }
  | { type: "context_compaction_started"; estimatedTokens: number }
  | {
      type: "context_compaction_completed";
      replacedThroughSequence: number;
      estimatedTokens: number;
    }
  | { type: "context_compaction_failed"; failureKind: CompactionFailureKind }
  | { type: "recovery_state_changed"; state: RecoveryState }
  | {
      /** Goal 状态变更事件的固定判别字段。 */
      type: "goal_changed";
      /** 当前 Goal 标识；与状态字段成对出现，清空时二者都省略。 */
      goalId?: string;
      /** 项目级 Goal 状态的单调修订号。 */
      revision: number;
      /** 当前 Goal 生命周期状态；与 Goal 标识成对出现。 */
      status?: "active" | "completed" | "blocked";
    }
  | { type: "system_notification"; level: AcpSystemNotificationLevel; message: string }
  | {
      type: "model_retry_scheduled";
      attempt: number;
      maxAttempts: number;
      delayMs: number;
      message: string;
    }
  | {
      type: "background_task_completed";
      taskId: string;
      taskKind: BackgroundTaskKind;
      agentId?: string;
      status: BackgroundTaskTerminalStatus;
      durationMs: number;
      summary?: string;
    }
  | { type: "model_first_stream_observed" };

/** KeenCode 生命周期事件的严格投递信封。 */
export interface KeenCodeEventEnvelope {
  /** 信封 Schema 版本，当前固定为 1。 */
  schemaVersion: typeof ACP_DELIVERY_SCHEMA_VERSION;
  /** 根 Session 稳定标识。 */
  sessionId: string;
  /** Turn 级事件的稳定 Turn 标识。 */
  turnId?: string;
  /** Turn 级事件的来源 Agent 标识。 */
  sourceAgentId?: string;
  /** 仅权威事件携带的 Session Journal 序号。 */
  journalSequence?: number;
  /** 当前 Runtime 内按 Session 单调递增的共享投递序号。 */
  deliverySequence: number;
  /** 事件发生时的 UTC Unix 毫秒时间。 */
  occurredAtMs: number;
  /** 类型化生命周期事件。 */
  event: KeenCodeEvent;
}

/** 两个共享投递水位的信封联合。 */
export type AcpDeliveryEnvelope =
  | SessionUpdateDeliveryEnvelope
  | KeenCodeEventEnvelope;

/** 项目级 Goal 的当前唯一线格式。 */
export interface GoalRecordDto {
  /** Goal 稳定标识。 */
  id: string;
  /** 用户可见标题。 */
  title: string;
  /** 当前固定为项目作用域。 */
  scope: "project";
  /** Goal 生命周期状态。 */
  status: "active" | "completed" | "blocked";
  /** 可选补充说明。 */
  description?: string;
  /** 可选人工进度百分比。 */
  progressPercent?: number;
  /** 完整目标描述。 */
  objective: string;
  /** 可选 Token 预算。 */
  tokenBudget?: number;
  /** 已累计使用的 Token。 */
  tokensUsed: number;
  /** 已累计执行秒数。 */
  timeUsedSeconds: number;
  /** 阻塞状态的安全原因。 */
  blockedReason?: string;
  /** 完成状态的非空验收证据。 */
  completionEvidence?: string;
  /** 创建时的 UTC Unix 毫秒时间。 */
  createdAtMs: number;
  /** 最近变化时的 UTC Unix 毫秒时间。 */
  updatedAtMs: number;
}

/** ACP JSON-RPC 请求标识。 */
export type AcpJsonRpcId = string | number;

/** `elicitation/create` 的完整标准 JSON-RPC 请求。 */
export interface AcpElicitationClientRequest {
  /** JSON-RPC 协议版本。 */
  jsonrpc: "2.0";
  /** Runtime 分配的请求标识。 */
  id: AcpJsonRpcId;
  /** 标准 ACP 结构化问答方法。 */
  method: "elicitation/create";
  /** 当前仅支持 Session 作用域的表单问答。 */
  params: {
    /** 当前仅支持表单模式。 */
    mode: "form";
    /** 目标 Session 标识。 */
    sessionId: string;
    /** 可选关联工具调用。 */
    toolCallId?: string;
    /** 用户可见说明正文。 */
    message: string;
    /** ACP 标准表单 JSON Schema。 */
    requestedSchema: {
      /** 表单 Schema 根类型固定为对象。 */
      type: "object";
      /** 表单属性集合。 */
      properties: Record<string, unknown>;
      /** 可选必填属性列表。 */
      required?: string[];
    };
    /** ACP 标准透传元数据。 */
    _meta?: Record<string, unknown>;
  };
}

/** Runtime 可以发送给桌面 Client 的标准 JSON-RPC 请求。 */
export type AcpJsonRpcClientRequest = AcpElicitationClientRequest;

/** Tauri 投递的 MCP OAuth JSON-RPC 通知；不包含 JSON-RPC request ID。 */
export interface McpOAuthNotification {
  /** JSON-RPC 协议版本。 */
  jsonrpc: "2.0";
  /** KeenCode Host 级 MCP OAuth 通知方法。 */
  method: "keencode/mcp/oauth";
  /** 项目作用域的 OAuth 生命周期事件。 */
  params: McpOAuthEvent;
}

/** Tauri 向前端发送的唯一事件载荷。 */
export type AcpTauriDelivery =
  | {
      /** 标准 SessionUpdate 投递。 */
      type: "session_update";
      /** 已由 Runtime 建立身份与顺序的标准更新信封。 */
      envelope: SessionUpdateDeliveryEnvelope;
    }
  | {
      /** KeenCode 扩展生命周期投递。 */
      type: "keencode_event";
      /** 已由 Runtime 建立身份与顺序的扩展事件信封。 */
      envelope: KeenCodeEventEnvelope;
    }
  | {
      /** Agent 发给桌面 Client 的 JSON-RPC 请求。 */
      type: "client_request";
      /** 完整请求信封，响应必须保留相同请求标识。 */
      request: AcpJsonRpcClientRequest;
    }
  | {
      /** 不绑定 Session 的 MCP OAuth Host 通知。 */
      type: "notification";
      /** 已严格校验的 MCP OAuth JSON-RPC 通知。 */
      notification: McpOAuthNotification;
    };

/** 判断未知值是否为普通对象。 */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 判断数字是否为正的安全整数。 */
function isPositiveInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) > 0;
}

/** 判断值是否为非空字符串。 */
function isIdentifier(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

/** 判断字符串是否包含 Rust `char::is_control` 对应的控制字符。 */
function hasControlCharacter(value: string): boolean {
  return /[\u0000-\u001F\u007F-\u009F]/u.test(value);
}

/** 返回字符串的 UTF-8 字节数，与 Rust 边界校验使用同一计量单位。 */
function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).length;
}

/** 判断值是否为有界且不含控制字符的事件标识。 */
function isEventIdentifier(
  value: unknown,
  maxBytes = MAX_EVENT_IDENTIFIER_BYTES,
): value is string {
  return typeof value === "string" && value.length > 0 &&
    utf8ByteLength(value) <= maxBytes && !hasControlCharacter(value);
}

/** 判断值是否为有界、非空且只含允许换行控制字符的事件文本。 */
function isEventText(
  value: unknown,
  maxBytes = MAX_EVENT_MESSAGE_BYTES,
): value is string {
  return typeof value === "string" && value.length > 0 &&
    utf8ByteLength(value) <= maxBytes &&
    ![...value].some((character) =>
      hasControlCharacter(character) && !["\n", "\r", "\t"].includes(character));
}

/** 判断值是否为 Rust `u64` 语义下可被前端精确表示的非负整数。 */
function isNonNegativeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) >= 0;
}

/** 判断可选字段是否确实省略，避免把 `null` 或 `undefined` 当作缺失。 */
function isOmittedOr(
  value: Record<string, unknown>,
  key: string,
  predicate: (candidate: unknown) => boolean,
): boolean {
  return !Object.hasOwn(value, key) || predicate(value[key]);
}

/** 判断值是否为 ACP 允许的 JSON-RPC 请求标识。 */
function isJsonRpcId(value: unknown): value is AcpJsonRpcId {
  return isIdentifier(value) ||
    (typeof value === "number" && Number.isSafeInteger(value));
}

/** 判断对象是否只含给定字段。 */
function hasOnlyKeys(
  value: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[] = [],
): boolean {
  const allowed = new Set([...required, ...optional]);
  return (
    required.every((key) => Object.hasOwn(value, key)) &&
    Object.keys(value).every((key) => allowed.has(key))
  );
}

/** 判断值是否属于 ACP 工具类别封闭枚举。 */
function isToolKind(value: unknown): value is AcpToolKind {
  return value === "read" || value === "edit" || value === "delete" ||
    value === "move" || value === "search" || value === "execute" ||
    value === "think" || value === "fetch" || value === "switch_mode" ||
    value === "other";
}

/** 判断值是否属于 ACP 工具状态封闭枚举。 */
function isToolStatus(value: unknown): value is AcpToolStatus {
  return value === "pending" || value === "in_progress" ||
    value === "completed" || value === "failed";
}

/** 判断值是否为非负有限数。 */
function isNonNegativeNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

/** 严格验证 ACP 内容块，未知字段由 `_meta` 承载。 */
function isContentBlock(value: unknown): value is AcpContentBlock {
  if (!isRecord(value) || typeof value.type !== "string" ||
    (value._meta !== undefined && !isRecord(value._meta))) return false;
  switch (value.type) {
    case "text":
      return hasOnlyKeys(value, ["type", "text"], ["_meta"]) &&
        typeof value.text === "string";
    case "image":
    case "audio":
      return hasOnlyKeys(value, ["type", "data", "mimeType"], ["_meta"]) &&
        typeof value.data === "string" && isIdentifier(value.mimeType);
    case "resource_link":
      return isStandardResourceLink(value) &&
        isValidFileChangeResourceLink(value);
    case "resource":
      return hasOnlyKeys(value, ["type", "resource"], ["_meta"]) &&
        isRecord(value.resource);
    default:
      return false;
  }
}

/**
 * 通用 resource_link 不要求 KeenCode 私有元数据；一旦声明文件变更命名空间，
 * 就必须通过完整引用和 URI 身份校验，不能把伪造引用送入 Store。
 */
function isValidFileChangeResourceLink(
  value: Record<string, unknown>,
): boolean {
  const meta = value._meta;
  if (!isRecord(meta) || !Object.hasOwn(meta, "keencode/fileChange")) {
    return true;
  }
  return parseFileChangeResourceLink(value) !== null;
}

/** 严格验证可选 `_meta` 字段。 */
function hasValidMeta(value: Record<string, unknown>): boolean {
  return value._meta === undefined || isRecord(value._meta);
}

/** 严格验证一条 ACP 计划项。 */
function isPlanEntry(value: unknown): value is PlanEntry {
  return isRecord(value) &&
    hasOnlyKeys(value, ["content", "priority", "status"], ["_meta"]) &&
    typeof value.content === "string" &&
    (value.priority === "high" || value.priority === "medium" ||
      value.priority === "low") &&
    (value.status === "pending" || value.status === "in_progress" ||
      value.status === "completed") && hasValidMeta(value);
}

/** 严格验证工具结果内容项。 */
function isToolContentItem(value: unknown): value is AcpToolCallContent {
  if (!isRecord(value) || !hasValidMeta(value)) return false;
  switch (value.type) {
    case "content":
      return hasOnlyKeys(value, ["type", "content"], ["_meta"]) &&
        isContentBlock(value.content);
    case "diff":
      return hasOnlyKeys(value, ["type", "path", "newText"], ["oldText", "_meta"]) &&
        typeof value.path === "string" && typeof value.newText === "string" &&
        (value.oldText === undefined || value.oldText === null ||
          typeof value.oldText === "string");
    case "terminal":
      return hasOnlyKeys(value, ["type", "terminalId"], ["_meta"]) &&
        isIdentifier(value.terminalId);
    default:
      return false;
  }
}

/** 严格验证一条可用命令。 */
function isAvailableCommand(value: unknown): boolean {
  return isRecord(value) &&
    hasOnlyKeys(value, ["name", "description"], ["input", "_meta"]) &&
    isIdentifier(value.name) && typeof value.description === "string" &&
    (value.input === undefined || value.input === null || isRecord(value.input)) &&
    hasValidMeta(value);
}

/** 判断事件摘要是否包含标签后未脱敏的凭据值。 */
function containsUnredactedLabeledValue(value: string, label: string): boolean {
  let offset = value.indexOf(label);
  while (offset >= 0) {
    let remainder = value.slice(offset + label.length).trimStart();
    if (remainder.startsWith('"') || remainder.startsWith("'")) {
      remainder = remainder.slice(1).trimStart();
    }
    if (remainder.startsWith(":") || remainder.startsWith("=")) {
      const candidate = remainder.slice(1).trimStart()
        .replace(/^["' ]+/u, "");
      if (candidate.length > 0 &&
        !candidate.startsWith("[redacted]") &&
        !candidate.startsWith("<redacted>") &&
        !candidate.startsWith("[已脱敏]") &&
        !candidate.startsWith("bearer [redacted]") &&
        !candidate.startsWith("basic [redacted]") &&
        !candidate.startsWith("***")) {
        return true;
      }
    }
    offset = value.indexOf(label, offset + 1);
  }
  return false;
}

/** 判断事件摘要是否包含足以构成凭据的固定前缀 Token。 */
function containsPrefixedSecret(value: string, prefix: string): boolean {
  let offset = value.indexOf(prefix);
  while (offset >= 0) {
    const match = value.slice(offset).match(/^[a-z0-9_-]+/u);
    if (match && match[0].length >= 20) return true;
    offset = value.indexOf(prefix, offset + 1);
  }
  return false;
}

/** 严格验证有界且已脱敏的后台任务展示摘要。 */
function isRedactedSummary(value: unknown): value is string {
  if (!isEventText(value)) return false;
  const normalized = value.toLowerCase();
  const sensitiveLabels = [
    "authorization",
    "x-api-key",
    "api-key",
    "api_key",
    "apikey",
    "client_secret",
    "access_token",
    "refresh_token",
    "id_token",
    "cookie",
  ];
  return !sensitiveLabels.some((label) =>
    containsUnredactedLabeledValue(normalized, label)) &&
    !containsPrefixedSecret(normalized, "sk-");
}

/** 判断 OAuth 授权地址是否为安全 HTTPS 或本机回环 HTTP。 */
function isSafeOAuthAuthorizationUrl(value: unknown): value is string {
  if (!isEventText(value, MAX_OAUTH_AUTHORIZATION_URL_BYTES) ||
    [...value].some((character) => /\s/u.test(character))) {
    return false;
  }
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return false;
  }
  if (parsed.username || parsed.password || value.includes("#")) return false;
  const isHttps = parsed.protocol === "https:";
  const isLocalHttp = parsed.protocol === "http:" &&
    isLoopbackHost(parsed.hostname);
  if (!isHttps && !isLocalHttp) return false;
  for (const [name] of parsed.searchParams) {
    if ([
      "access_token",
      "refresh_token",
      "id_token",
      "client_secret",
      "api_key",
      "apikey",
      "authorization",
    ].includes(name.toLowerCase())) {
      return false;
    }
  }
  return true;
}

/** 判断 URL 主机是否为 localhost、127/8 或 IPv6 回环地址。 */
function isLoopbackHost(host: string): boolean {
  const normalized = host.toLowerCase();
  if (normalized === "localhost" || normalized.endsWith(".localhost")) {
    return true;
  }
  const ipv4 = normalized.split(".");
  if (ipv4.length === 4 && ipv4.every((part) => /^\d+$/u.test(part))) {
    const octets = ipv4.map(Number);
    if (octets.every((octet) => octet >= 0 && octet <= 255)) {
      return octets[0] === 127;
    }
  }
  return normalized === "[::1]" || normalized === "::1";
}

/** 判断更新是否属于不绑定 Turn 的 Session 级标准更新。 */
export function isSessionScopedUpdate(update: SessionUpdate): boolean {
  switch (update.sessionUpdate) {
    case "available_commands_update":
    case "current_mode_update":
    case "config_option_update":
    case "plan":
    case "session_info_update":
      return true;
    default:
      return false;
  }
}

/** 判断事件是否允许作为不绑定 Turn 的 Session 级生命周期事件。 */
export function isSessionScopedKeenCodeEvent(event: KeenCodeEvent): boolean {
  switch (event.type) {
    case "recovery_state_changed":
    case "goal_changed":
    case "background_task_completed":
    case "system_notification":
      return true;
    default:
      return false;
  }
}

/** 判断事件是否必须来自权威 Session Journal。 */
export function isAuthoritativeKeenCodeEvent(event: KeenCodeEvent): boolean {
  switch (event.type) {
    case "turn_started":
    case "turn_completed":
    case "turn_cancelled":
    case "turn_failed":
    case "agent_spawned":
    case "agent_status_changed":
    case "agent_message_queued":
    case "context_compaction_completed":
      return true;
    default:
      return false;
  }
}

/** 判断事件是否结束一个 Turn。 */
export function isTerminalKeenCodeEvent(event: KeenCodeEvent): boolean {
  return (
    event.type === "turn_completed" ||
    event.type === "turn_cancelled" ||
    event.type === "turn_failed"
  );
}

/** 判断标准更新判别字段是否属于当前封闭联合。 */
function isSessionUpdate(value: unknown): value is SessionUpdate {
  if (!isRecord(value) || typeof value.sessionUpdate !== "string") return false;
  switch (value.sessionUpdate) {
    case "user_message_chunk":
    case "agent_message_chunk":
    case "agent_thought_chunk":
      return hasOnlyKeys(
        value,
        ["sessionUpdate", "content"],
        ["_meta"],
      ) && isContentBlock(value.content) &&
        hasValidMeta(value);
    case "tool_call":
      return hasOnlyKeys(
        value,
        ["sessionUpdate", "toolCallId", "title"],
        ["kind", "status", "rawInput", "content", "_meta"],
      ) && isIdentifier(value.toolCallId) && typeof value.title === "string" &&
        (value.kind === undefined || isToolKind(value.kind)) &&
        (value.status === undefined || isToolStatus(value.status)) &&
        (value.content === undefined ||
          (Array.isArray(value.content) && value.content.every(isToolContentItem))) &&
        hasValidToolOutcome(value);
    case "tool_call_update":
      return hasOnlyKeys(
        value,
        ["sessionUpdate", "toolCallId"],
        ["kind", "status", "title", "content", "rawOutput", "_meta"],
      ) && isIdentifier(value.toolCallId) &&
        (value.kind === undefined || isToolKind(value.kind)) &&
        (value.status === undefined || isToolStatus(value.status)) &&
        (value.title === undefined || typeof value.title === "string") &&
        (value.content === undefined ||
          (Array.isArray(value.content) && value.content.every(isToolContentItem))) &&
        hasValidToolOutcome(value);
    case "plan":
      return hasOnlyKeys(value, ["sessionUpdate", "entries"], ["_meta"]) &&
        Array.isArray(value.entries) && value.entries.every(isPlanEntry) &&
        hasValidMeta(value);
    case "usage_update":
      return hasOnlyKeys(
        value,
        ["sessionUpdate", "used", "size"],
        ["cost", "_meta"],
      ) && isNonNegativeInteger(value.used) && isPositiveInteger(value.size) &&
        (value.cost === undefined ||
          (isRecord(value.cost) &&
            hasOnlyKeys(value.cost, ["amount", "currency"]) &&
            isNonNegativeNumber(value.cost.amount) &&
            isIdentifier(value.cost.currency))) && hasValidMeta(value);
    case "available_commands_update":
      return hasOnlyKeys(
        value,
        ["sessionUpdate", "availableCommands"],
        ["_meta"],
      ) && Array.isArray(value.availableCommands) &&
        value.availableCommands.every(isAvailableCommand) && hasValidMeta(value);
    case "current_mode_update":
      return hasOnlyKeys(
        value,
        ["sessionUpdate", "currentModeId"],
        ["_meta"],
      ) && isIdentifier(value.currentModeId) && hasValidMeta(value);
    case "config_option_update":
      return hasOnlyKeys(
        value,
        ["sessionUpdate", "configOptions"],
        ["_meta"],
      ) && Array.isArray(value.configOptions) &&
        value.configOptions.every(isRecord) && hasValidMeta(value);
    case "session_info_update":
      return hasOnlyKeys(
        value,
        ["sessionUpdate"],
        ["title", "updatedAt", "_meta"],
      ) && (value.title === undefined || value.title === null ||
        typeof value.title === "string") &&
        (value.updatedAt === undefined || value.updatedAt === null ||
          typeof value.updatedAt === "string") && hasValidMeta(value);
    default:
      return false;
  }
}

/** 判断 KeenCode 事件判别字段是否属于当前封闭联合。 */
function isKeenCodeEvent(value: unknown): value is KeenCodeEvent {
  if (!isRecord(value) || typeof value.type !== "string") return false;
  switch (value.type) {
    case "turn_completed":
    case "turn_cancelled":
    case "model_first_stream_observed":
      return hasOnlyKeys(value, ["type"]);
    case "turn_started":
      return hasOnlyKeys(value, ["type", "rootTurnId"], ["parentTurnId"]) &&
        isEventIdentifier(value.rootTurnId) &&
        isOmittedOr(value, "parentTurnId", isEventIdentifier);
    case "turn_failed":
      return hasOnlyKeys(value, ["type", "failureKind", "message"]) &&
        (value.failureKind === "model" || value.failureKind === "context" ||
          value.failureKind === "tool" || value.failureKind === "storage" ||
          value.failureKind === "internal") && isEventText(value.message);
    case "agent_spawned":
      return hasOnlyKeys(value, [
        "type", "agentId", "parentAgentId", "agentPath", "task", "parentTurnId", "rootTurnId",
      ]) && isEventIdentifier(value.agentId) &&
        isEventIdentifier(value.parentAgentId) &&
        isEventIdentifier(value.agentPath, MAX_AGENT_PATH_BYTES) &&
        isEventText(value.task, MAX_AGENT_TASK_BYTES) &&
        isEventIdentifier(value.parentTurnId) &&
        isEventIdentifier(value.rootTurnId) &&
        value.agentId !== value.parentAgentId &&
        value.parentTurnId === value.rootTurnId;
    case "agent_status_changed":
      return hasOnlyKeys(value, ["type", "agentId", "status"]) &&
        isEventIdentifier(value.agentId) &&
        (value.status === "pending" || value.status === "running" ||
          value.status === "waiting" || value.status === "completed" ||
          value.status === "interrupted" || value.status === "failed" ||
          value.status === "stopped");
    case "agent_message_queued":
      return hasOnlyKeys(
        value,
        ["type", "messageId", "fromAgentId", "toAgentId"],
      ) && isEventIdentifier(value.messageId) &&
        isEventIdentifier(value.fromAgentId) &&
        isEventIdentifier(value.toAgentId) &&
        value.fromAgentId !== value.toAgentId;
    case "context_compaction_started":
      return hasOnlyKeys(value, ["type", "estimatedTokens"]) &&
        isNonNegativeInteger(value.estimatedTokens);
    case "context_compaction_completed":
      return hasOnlyKeys(
        value,
        ["type", "replacedThroughSequence", "estimatedTokens"],
      ) && Number.isSafeInteger(value.replacedThroughSequence) &&
        Number(value.replacedThroughSequence) > 0 &&
        isNonNegativeInteger(value.estimatedTokens);
    case "context_compaction_failed":
      return hasOnlyKeys(value, ["type", "failureKind"]) &&
        (value.failureKind === "model" || value.failureKind === "budget" ||
          value.failureKind === "storage" || value.failureKind === "invalid_result");
    case "recovery_state_changed":
      return hasOnlyKeys(value, ["type", "state"]) &&
        (value.state === "pending" || value.state === "replaying" ||
          value.state === "ready" || value.state === "failed");
    case "goal_changed":
      return hasOnlyKeys(value, ["type", "revision"], ["goalId", "status"]) &&
        isPositiveInteger(value.revision) &&
        isOmittedOr(value, "goalId", isEventIdentifier) &&
        isOmittedOr(value, "status", (status) =>
          status === "active" || status === "completed" || status === "blocked") &&
        (Object.hasOwn(value, "goalId") === Object.hasOwn(value, "status"));
    case "system_notification":
      return hasOnlyKeys(value, ["type", "level", "message"]) &&
        (value.level === "info" || value.level === "warning" ||
          value.level === "error") && isEventText(value.message);
    case "model_retry_scheduled":
      return hasOnlyKeys(
        value,
        ["type", "attempt", "maxAttempts", "delayMs", "message"],
      ) && isPositiveInteger(value.attempt) &&
        isPositiveInteger(value.maxAttempts) &&
        Number(value.attempt) < Number(value.maxAttempts) &&
        Number(value.maxAttempts) <= MAX_MODEL_RETRY_ATTEMPTS &&
        isNonNegativeInteger(value.delayMs) &&
        Number(value.delayMs) <= MAX_MODEL_RETRY_DELAY_MS &&
        isEventText(value.message);
    case "background_task_completed":
      return hasOnlyKeys(
        value,
        ["type", "taskId", "taskKind", "status", "durationMs"],
        ["agentId", "summary"],
      ) && isEventIdentifier(value.taskId) &&
        (value.taskKind === "shell" || value.taskKind === "agent") &&
        (value.status === "succeeded" || value.status === "failed" ||
          value.status === "cancelled") &&
        isNonNegativeInteger(value.durationMs) &&
        Number(value.durationMs) <= MAX_BACKGROUND_TASK_DURATION_MS &&
        ((value.taskKind === "agent" &&
          isOmittedOr(value, "agentId", isEventIdentifier) &&
          Object.hasOwn(value, "agentId")) ||
          (value.taskKind === "shell" && !Object.hasOwn(value, "agentId"))) &&
        isOmittedOr(value, "summary", isRedactedSummary);
    default:
      return false;
  }
}

/** 校验事件与信封 Turn、Agent 和 Journal 序号之间的跨字段不变量。 */
function isKeenCodeEventIdentity(
  event: KeenCodeEvent,
  turnId: string | undefined,
  sourceAgentId: string | undefined,
  journalSequence: number | undefined,
): boolean {
  const sessionScoped = isSessionScopedKeenCodeEvent(event);
  if (sessionScoped) {
    if (event.type === "system_notification") {
      return (turnId === undefined && sourceAgentId === undefined) ||
        (turnId !== undefined && sourceAgentId !== undefined);
    }
    return turnId === undefined && sourceAgentId === undefined;
  }
  if (turnId === undefined || sourceAgentId === undefined) return false;
  switch (event.type) {
    case "turn_started":
      return event.parentTurnId === undefined
        ? event.rootTurnId === turnId
        : event.parentTurnId !== turnId && event.rootTurnId !== turnId &&
          event.parentTurnId === event.rootTurnId;
    case "agent_spawned":
      return event.parentAgentId === sourceAgentId &&
        event.parentTurnId === turnId;
    case "agent_status_changed":
      return event.agentId === sourceAgentId;
    case "agent_message_queued":
      return event.fromAgentId === sourceAgentId;
    case "context_compaction_completed":
      return journalSequence !== undefined &&
        event.replacedThroughSequence < journalSequence;
    default:
      return true;
  }
}

/** 严格读取标准 SessionUpdate 投递信封；旧包装和未知顶层字段直接拒绝。 */
export function parseSessionUpdateDeliveryEnvelope(
  value: unknown,
): SessionUpdateDeliveryEnvelope | null {
  if (!isRecord(value) || !hasOnlyKeys(value, [
    "schemaVersion", "sessionId", "deliverySequence", "occurredAtMs", "update",
  ], ["turnId", "sourceAgentId"]) ||
    value.schemaVersion !== ACP_DELIVERY_SCHEMA_VERSION ||
    !isEventIdentifier(value.sessionId) || !isPositiveInteger(value.deliverySequence) ||
    !isPositiveInteger(value.occurredAtMs) || !isSessionUpdate(value.update)) {
    return null;
  }
  const hasTurnId = isEventIdentifier(value.turnId);
  const hasSourceAgentId = isEventIdentifier(value.sourceAgentId);
  if (Object.hasOwn(value, "turnId") !== hasTurnId ||
    Object.hasOwn(value, "sourceAgentId") !== hasSourceAgentId) return null;
  if (hasTurnId !== hasSourceAgentId) return null;
  // 历史独立用户消息可以没有 Turn；当前回合回放的用户消息也可以带有完整身份，
  // 因此 user_message_chunk 是唯一同时允许两种作用域的标准更新。
  const userMessage = value.update.sessionUpdate === "user_message_chunk";
  if (!userMessage && isSessionScopedUpdate(value.update) === hasTurnId) return null;
  return value as unknown as SessionUpdateDeliveryEnvelope;
}

/** 严格读取 KeenCode 事件投递信封；不接受二次 JSON 字符串或旧事件别名。 */
export function parseKeenCodeEventEnvelope(
  value: unknown,
): KeenCodeEventEnvelope | null {
  if (!isRecord(value) || !hasOnlyKeys(value, [
    "schemaVersion", "sessionId", "deliverySequence", "occurredAtMs", "event",
  ], ["turnId", "sourceAgentId", "journalSequence"]) ||
    value.schemaVersion !== ACP_DELIVERY_SCHEMA_VERSION ||
    !isEventIdentifier(value.sessionId) || !isPositiveInteger(value.deliverySequence) ||
    !isPositiveInteger(value.occurredAtMs) || !isKeenCodeEvent(value.event)) {
    return null;
  }
  const hasTurnId = isEventIdentifier(value.turnId);
  const hasSourceAgentId = isEventIdentifier(value.sourceAgentId);
  if (Object.hasOwn(value, "turnId") !== hasTurnId ||
    Object.hasOwn(value, "sourceAgentId") !== hasSourceAgentId) return null;
  if (hasTurnId !== hasSourceAgentId) return null;
  const sessionScoped = isSessionScopedKeenCodeEvent(value.event);
  if ((sessionScoped && value.event.type !== "system_notification" && hasTurnId) ||
    (!sessionScoped && !hasTurnId)) return null;
  const authoritative = isAuthoritativeKeenCodeEvent(value.event);
  const hasJournalSequence = isPositiveInteger(value.journalSequence);
  if (Object.hasOwn(value, "journalSequence") !== hasJournalSequence) return null;
  if (authoritative !== hasJournalSequence) return null;
  const turnId = hasTurnId ? value.turnId as string : undefined;
  const sourceAgentId = hasSourceAgentId ? value.sourceAgentId as string : undefined;
  const journalSequence = hasJournalSequence
    ? value.journalSequence as number
    : undefined;
  if (!isKeenCodeEventIdentity(
    value.event,
    turnId,
    sourceAgentId,
    journalSequence,
  )) return null;
  return value as unknown as KeenCodeEventEnvelope;
}

/** 严格读取当前支持的 Session 表单问答 Client 请求。 */
function parseElicitationClientRequest(
  value: Record<string, unknown>,
): AcpElicitationClientRequest | null {
  if (!hasOnlyKeys(value, ["jsonrpc", "id", "method", "params"]) ||
    value.jsonrpc !== "2.0" || !isJsonRpcId(value.id) ||
    value.method !== "elicitation/create" || !isRecord(value.params)) {
    return null;
  }
  const params = value.params;
  if (!hasOnlyKeys(
    params,
    ["mode", "sessionId", "message", "requestedSchema"],
    ["toolCallId", "_meta"],
  ) || params.mode !== "form" || !isIdentifier(params.sessionId) ||
    typeof params.message !== "string" || !isRecord(params.requestedSchema) ||
    (params.toolCallId !== undefined && !isIdentifier(params.toolCallId)) ||
    (params._meta !== undefined && !isRecord(params._meta))) {
    return null;
  }
  const schema = params.requestedSchema;
  if (schema.type !== "object" || !isRecord(schema.properties) ||
    (schema.required !== undefined &&
      (!Array.isArray(schema.required) ||
        !schema.required.every(isIdentifier)))) {
    return null;
  }
  return value as unknown as AcpElicitationClientRequest;
}

/** 严格读取 Runtime 发给桌面 Client 的完整 JSON-RPC 请求。 */
export function parseAcpJsonRpcClientRequest(
  value: unknown,
): AcpJsonRpcClientRequest | null {
  if (!isRecord(value)) return null;
  return parseElicitationClientRequest(value);
}

/** 严格读取不绑定 Session 的 MCP OAuth JSON-RPC 通知。 */
export function parseMcpOAuthNotification(
  value: unknown,
): McpOAuthNotification | null {
  if (!isRecord(value) || !hasOnlyKeys(value, ["jsonrpc", "method", "params"]) ||
    value.jsonrpc !== "2.0" || value.method !== "keencode/mcp/oauth" ||
    !isRecord(value.params)) {
    return null;
  }
  const event = value.params;
  switch (event.type) {
    case "mcp_oauth_authorization_required":
      if (!hasOnlyKeys(
        event,
        ["type", "projectPath", "serverName", "authorizationUrl"],
      ) || !isEventIdentifier(event.projectPath, MAX_OAUTH_PROJECT_PATH_BYTES) ||
        !isEventIdentifier(event.serverName) ||
        !isSafeOAuthAuthorizationUrl(event.authorizationUrl)) {
        return null;
      }
      break;
    case "mcp_oauth_authorized":
      if (!hasOnlyKeys(event, ["type", "projectPath", "serverName"]) ||
        !isEventIdentifier(event.projectPath, MAX_OAUTH_PROJECT_PATH_BYTES) ||
        !isEventIdentifier(event.serverName)) {
        return null;
      }
      break;
    case "mcp_oauth_failed":
      if (!hasOnlyKeys(event, ["type", "projectPath", "serverName", "message"]) ||
        !isEventIdentifier(event.projectPath, MAX_OAUTH_PROJECT_PATH_BYTES) ||
        !isEventIdentifier(event.serverName) || !isEventText(event.message)) {
        return null;
      }
      break;
    default:
      return null;
  }
  return value as unknown as McpOAuthNotification;
}

/** 严格读取唯一 `acp://delivery` Tauri 载荷。 */
export function parseAcpTauriDelivery(value: unknown): AcpTauriDelivery | null {
  if (!isRecord(value) || typeof value.type !== "string") return null;
  switch (value.type) {
    case "session_update": {
      if (!hasOnlyKeys(value, ["type", "envelope"])) return null;
      const envelope = parseSessionUpdateDeliveryEnvelope(value.envelope);
      return envelope ? { type: value.type, envelope } : null;
    }
    case "keencode_event": {
      if (!hasOnlyKeys(value, ["type", "envelope"])) return null;
      const envelope = parseKeenCodeEventEnvelope(value.envelope);
      return envelope ? { type: value.type, envelope } : null;
    }
    case "client_request": {
      if (!hasOnlyKeys(value, ["type", "request"])) return null;
      const request = parseAcpJsonRpcClientRequest(value.request);
      return request ? { type: value.type, request } : null;
    }
    case "notification": {
      if (!hasOnlyKeys(value, ["type", "notification"])) return null;
      const notification = parseMcpOAuthNotification(value.notification);
      return notification ? { type: value.type, notification } : null;
    }
    default:
      return null;
  }
}

/** 判断实时更新是否应驱动根 Agent 的 streaming 状态。 */
export function shouldDriveMainSessionStreaming(
  update: SessionUpdate,
  sourceIsChild: boolean,
): boolean {
  if (sourceIsChild) return false;
  switch (update.sessionUpdate) {
    case "user_message_chunk":
    case "agent_message_chunk":
    case "agent_thought_chunk":
    case "tool_call":
    case "tool_call_update":
      return true;
    default:
      return false;
  }
}

/** 合并同一时间线连续文本分片，避免每个 Token 都扩容实时字符串。 */
export function mergeSessionTextUpdates(
  current: SessionUpdate | undefined,
  next: SessionUpdate,
): AgentMessageChunkUpdate | AgentThoughtChunkUpdate | null {
  if (next.sessionUpdate !== "agent_message_chunk" &&
    next.sessionUpdate !== "agent_thought_chunk") return null;
  if (next.content.type !== "text") return null;
  if (!current) return next;
  if (current.sessionUpdate !== next.sessionUpdate || current.content.type !== "text") {
    return null;
  }
  return {
    ...next,
    content: { ...next.content, text: current.content.text + next.content.text },
  };
}

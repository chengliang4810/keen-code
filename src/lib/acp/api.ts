/** ACP 后端命令包装（前端 → Tauri invoke）。

invoke 命令名与后端 session_commands.rs 注册的一致；事件监听封装在
listenAcp() 中，把 acp://* Tauri 事件解析为可判别通知。
 */

import type {
  AgentDoneEnvelope,
  AgentEventEnvelope,
  ElicitationEnvelope,
  GoalRecordDto,
  RecoveryEnvelope,
  SessionUpdateEnvelope,
} from "./events";

/** KeenCode 当前会转发给界面的 ACP 事件及其唯一载荷。 */
export interface AcpEventPayloads {
  "acp://session-update": SessionUpdateEnvelope;
  "acp://agent-event": AgentEventEnvelope;
  "acp://recovery-status": RecoveryEnvelope;
  "acp://elicitation": ElicitationEnvelope;
  "acp://agent-done": AgentDoneEnvelope;
}

export interface SessionSnapshot {
  sessionId?: string | null;
  state:
    | "idle"
    | "connecting"
    | "ready"
    | "streaming"
    | "disconnected";
  backend: "peri_acp";
  projectPath?: string | null;
  title?: string | null;
  lastError?: string | null;
  /** 后端诊断日志绝对路径。 */
  diagnosticsPath?: string | null;
}

/** peri ThreadStore 返回的当前 Session 列表项。 */
export interface SessionListItem {
  /** Session 稳定标识。 */
  id: string;
  /** Session 原始标题，尚未生成时为空。 */
  title: string | null;
  /** Session 工作目录。 */
  cwd: string;
  /** RFC 3339 最近更新时间。 */
  updatedAt: string;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function sessionGetState(): Promise<SessionSnapshot> {
  return invoke<SessionSnapshot>("session_get_state");
}

/** 返回后端诊断日志路径，供启动门禁和错误页展示。 */
export function diagnosticsLogPath(): Promise<string> {
  return invoke<string>("diagnostics_log_path");
}

/** 将前端 IPC 失败摘要写入后端诊断日志。 */
export function diagnosticsRecord(component: string, message: string): Promise<void> {
  return invoke<void>("diagnostics_record", { component, message });
}

export function sessionConnect(args: {
  projectPath?: string;
  sessionId?: string | null;
}): Promise<SessionSnapshot> {
  return invoke<SessionSnapshot>("session_connect", {
    projectPath: args.projectPath ?? null,
    sessionId: args.sessionId ?? null,
  });
}

export function sessionSend(args: {
  text: string;
  sessionId: string;
}): Promise<SessionSnapshot> {
  return invoke<SessionSnapshot>("session_send", args);
}

export function sessionStop(sessionId: string): Promise<SessionSnapshot> {
  return invoke<SessionSnapshot>("session_stop", { sessionId });
}

export function sessionFork(args: {
  sourceId: string;
  title?: string | null;
}): Promise<{ id: string }> {
  return invoke<{ id: string }>("session_fork", args);
}

export function sessionRename(id: string, title: string): Promise<SessionSnapshot> {
  return invoke<SessionSnapshot>("session_rename", { id, title });
}

/** 切换会话级模型（Q1 决策：每会话独立 provider，不影响新会话默认值）。 */
export function sessionSetModel(args: {
  sessionId: string;
  providerId: string;
  modelId: string;
}): Promise<void> {
  return invoke<void>("session_set_model", args);
}

/** 使用当前 Session 供应商概括首个成功回合，返回未净化的标题候选。 */
export function sessionGenerateTitle(args: {
  /** 目标 Session 标识。 */
  id: string;
  /** 首轮用户消息。 */
  userMessage: string;
  /** 首轮 Assistant 回复。 */
  assistantMessage: string;
}): Promise<string> {
  return invoke<string>("session_generate_title", args);
}

export function sessionMessages(id: string): Promise<unknown[]> {
  return invoke<unknown[]>("session_messages", { id });
}

/** 永久删除一个已停止的根 Session 及其全部消息。 */
export function sessionDelete(id: string): Promise<void> {
  return invoke<void>("session_delete", { id });
}

export function sessionDisconnect(): Promise<SessionSnapshot> {
  return invoke<SessionSnapshot>("session_disconnect");
}

export function sessionResolveAskUser(args: {
  rpcId: number;
  decision: "accepted" | "cancelled";
  answers?: unknown;
}): Promise<void> {
  return invoke<void>("session_resolve_ask_user", {
    rpcId: args.rpcId,
    decision: args.decision,
    answers: args.answers ?? null,
  });
}

export interface GoalListResult {
  sessionId: string;
  revision: number;
  goals: GoalRecordDto[];
  activeGoalId: string | null;
}

/** 返回当前 Session 列表。 */
export function sessionsList(): Promise<SessionListItem[]> {
  return invoke<SessionListItem[]>("sessions_list");
}

export function goalsList(sessionId: string): Promise<GoalListResult> {
  return invoke<GoalListResult>("goals_list", { sessionId });
}

export function goalUpsert(args: {
  sessionId: string;
  goal: Partial<GoalRecordDto> & { title: string };
  expectedRevision?: number;
  requestNonce?: string;
}): Promise<{ revision: number; goal: GoalRecordDto; deduplicated: boolean }> {
  return invoke("goal_upsert", args);
}

export function goalTransition(args: {
  sessionId: string;
  goalId: string;
  status: GoalRecordDto["status"];
  reason?: string | null;
  expectedRevision?: number;
  requestNonce?: string;
}): Promise<{ revision: number; goal: GoalRecordDto; deduplicated: boolean }> {
  return invoke("goal_transition", args);
}

/** 清除当前 Session 的持久 Goal。 */
export function goalClear(sessionId: string): Promise<{
  sessionId: string;
  revision: number;
  cleared: boolean;
}> {
  return invoke("goal_clear", { sessionId });
}

export interface ReplayResult {
  session_id: string;
  from: { epoch: string; sequence: number } | null;
  next: { epoch: string; sequence: number };
  replayed_events: number;
  truncated: boolean;
  status: "ok";
}

export function sessionReplay(args: {
  sessionId: string;
  after?: { epoch: string; sequence: number } | null;
  limit?: number;
}): Promise<ReplayResult> {
  return invoke<ReplayResult>("session_replay", args);
}

/** 订阅一个 acp://* Tauri 事件。返回取消函数。 */
export async function listenAcp<EventName extends keyof AcpEventPayloads>(
  event: EventName,
  handler: (notification: AcpEventPayloads[EventName]) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  const unlisten = await listen<AcpEventPayloads[EventName]>(event, (e) => {
    handler(e.payload);
  });
  return unlisten;
}

/** ACP 后端命令包装（前端 → Tauri invoke）。 */

import type {
  AcpJsonRpcId,
  AcpTauriDelivery,
  GoalRecordDto,
} from "./events";
import { invoke } from "../tauri";
import { acpInitialize, acpNotify, acpRequest, acpRespond } from "./client";
import { startSessionPrompt } from "./prompt";

/** KeenCode 当前只通过一个串行 Tauri 事件向界面投递 ACP 数据。 */
export interface AcpEventPayloads {
  /** 标准更新、KeenCode 事件和 Client 请求的唯一有序通道。 */
  "acp://delivery": AcpTauriDelivery;
}

/** 为一次可重试的用户操作生成稳定标识；调用方必须在重试期间复用返回值。 */
export function createOperationId(scope: string): string {
  if (!scope || scope.trim() !== scope || /\s/.test(scope)) {
    throw new Error("operationId scope 必须为非空且不含空白的字符串");
  }
  return `${scope}-${globalThis.crypto.randomUUID()}`;
}

export interface SessionSnapshot {
  sessionId?: string | null;
  state:
    | "idle"
    | "connecting"
    | "ready"
    | "streaming"
    | "disconnected";
  /** Host 当前唯一运行中的前台回合。 */
  activeTurnId: string | null;
  /** 当前唯一进程内协议后端。 */
  backend: "acp";
  projectPath?: string | null;
  title?: string | null;
  lastError?: string | null;
  /** 后端诊断日志绝对路径。 */
  diagnosticsPath?: string | null;
}

/** 权威 TurnStarted 事件确认的回合起点，不是后端私有响应。 */
export interface SessionPromptStarted {
  /** 已经实际启动的回合标识。 */
  turnId: string;
  /** 权威事件记录的 Unix Epoch 毫秒。 */
  occurredAtMs: number;
}

/** 标准 session/prompt 在根回合结束后返回的结果。 */
export interface SessionPromptResult {
  /** ACP 定义的停止原因。 */
  stopReason: "end_turn" | "max_tokens" | "max_turn_requests" | "refusal" | "cancelled";
  /** 宿主附加的命名空间元数据。 */
  _meta?: Record<string, unknown>;
}

/** 同一标准 Prompt 的实时起点和终态；两者不得互相冒充。 */
export interface SessionPromptRun {
  /** 由对应 Session/Turn 的实际事件完成。 */
  started: Promise<SessionPromptStarted>;
  /** 由标准 JSON-RPC Prompt 响应完成。 */
  completed: Promise<SessionPromptResult>;
}

/** `keencode/session/rewind` 完成后返回的 Session 回退结果。 */
export interface SessionRewindResult {
  /** 被回退的源 Session 标识。 */
  sessionId: string;
  /** 回退前完整历史所在的归档 Session 标识。 */
  archivedSessionId: string;
  /** 回退完成后权威 Journal 的最后序号。 */
  throughJournalSequence: number;
  /** 首版固定为 false，不自动恢复项目文件。 */
  revertedFiles: false;
}

/** 标准 Session 列表投影。 */
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

/** 返回后端诊断日志路径，供启动门禁和错误页展示。 */
export function diagnosticsLogPath(): Promise<string> {
  return invoke<string>("diagnostics_log_path");
}

/** 将前端 IPC 失败摘要写入后端诊断日志。 */
export function diagnosticsRecord(component: string, message: string): Promise<void> {
  return invoke<void>("diagnostics_record", { component, message });
}

export async function sessionConnect(args: {
  projectPath?: string;
  sessionId?: string | null;
  /** 新建 Session 时用于确定性对账，调用重试必须复用。 */
  operationId: string;
}): Promise<SessionSnapshot> {
  if (args.sessionId) return sessionSnapshotFromResult(await sessionLoad(args.sessionId));
  const initialization = await acpInitialize();
  const cwd = args.projectPath ?? initialization._meta?.["keencode/defaultCwd"];
  if (typeof cwd !== "string" || !cwd.trim()) {
    throw new Error("ACP Host 未提供新会话工作目录");
  }
  const result = await acpRequest<{ sessionId: string; _meta?: Record<string, unknown> }>(
    "session/new",
    { cwd, mcpServers: [], _meta: { "keencode/operationId": args.operationId } },
    args.operationId,
  );
  const snapshot = sessionSnapshotFromResult(result);
  if (result.sessionId !== snapshot.sessionId) throw new Error("ACP 新会话标识不一致");
  return snapshot;
}

/** 将本 Host 的命名空间快照投影为 UI 状态，不读取任何历史字段别名。 */
export function sessionSnapshotFromResult(result: { _meta?: Record<string, unknown> }): SessionSnapshot {
  const snapshot = result._meta?.["keencode/snapshot"];
  if (typeof snapshot !== "object" || snapshot === null || Array.isArray(snapshot)) {
    throw new Error("ACP 响应缺少 Session 快照");
  }
  const value = snapshot as Record<string, unknown>;
  if (
    typeof value.sessionId !== "string" || !value.sessionId ||
    value.backend !== "acp" ||
    !["idle", "ready", "streaming", "disconnected"].includes(String(value.state)) ||
    !(value.activeTurnId === null || typeof value.activeTurnId === "string") ||
    !(value.projectPath === null || typeof value.projectPath === "string") ||
    !(value.title === null || typeof value.title === "string") ||
    !(value.lastError === null || typeof value.lastError === "string")
  ) throw new Error("ACP Session 快照字段无效");
  return value as unknown as SessionSnapshot;
}

/** 发送一轮用户消息，并用 requestId 将终态通知与本轮请求严格配对。 */
export function sessionSend(args: {
  /** 发给 Agent 的完整文本。 */
  text: string;
  /** 目标根 Session 标识。 */
  sessionId: string;
  /** 本轮唯一且非空的请求标识。 */
  requestId: string;
  /** 计划模式：true 时后端在 developerContext 注入规划契约。 */
  planMode?: boolean;
  /** Ultra：true 时后端在 developerContext 注入主动委派契约。 */
  ultraMode?: boolean;
}): SessionPromptRun {
  return startSessionPrompt(args);
}

/** 将用户消息注入当前正在运行的回合。 */
export async function sessionSteer(args: {
  text: string;
  sessionId: string;
  /** 本次 mailbox 写入的业务幂等标识，放在 ACP 保留元数据中。 */
  operationId: string;
}): Promise<void> {
  await acpRequest("keencode/session/steer", {
    sessionId: args.sessionId,
    text: args.text,
    _meta: { "keencode/operationId": args.operationId },
  });
}

export function sessionStop(
  sessionId: string,
  requestId: string,
): Promise<void> {
  return acpNotify("session/cancel", {
    sessionId, _meta: { "keencode/turnId": requestId },
  });
}

export async function sessionFork(args: {
  sourceId: string;
  title?: string | null;
  /** 目标 Session 的确定性派生标识。 */
  operationId: string;
}): Promise<{ id: string }> {
  const cwd = await sessionCwd(args.sourceId);
  const result = await acpRequest<{ sessionId: string }>("session/fork", {
    sessionId: args.sourceId, cwd, mcpServers: [],
    _meta: {
      "keencode/operationId": args.operationId,
      ...(args.title == null ? {} : { "keencode/title": args.title }),
    },
  }, args.operationId);
  if (typeof result.sessionId !== "string" || !result.sessionId) {
    throw new Error("ACP Fork 未返回 Session 标识");
  }
  return { id: result.sessionId };
}

/** 将当前 Session 回退到指定用户消息，并保存回退前历史为归档 Session。 */
export function sessionRewind(args: {
  sessionId: string;
  /** 要删除的目标用户消息的后端稳定标识。 */
  targetMessageId: string;
  /** 目标用户消息的完整原始 Agent 文本，不做 trim。 */
  expectedText: string;
  /** 首版不自动恢复文件，固定为 false。 */
  revertFiles: false;
  /** rewind 事务的业务幂等标识，放在 ACP 保留元数据中。 */
  operationId: string;
}): Promise<SessionRewindResult> {
  return acpRequest<unknown>(
    "keencode/session/rewind",
    {
      sessionId: args.sessionId,
      targetMessageId: args.targetMessageId,
      expectedText: args.expectedText,
      revertFiles: false,
      _meta: { "keencode/operationId": args.operationId },
    },
  ).then((result) => {
    if (
      typeof result !== "object" ||
      result === null ||
      Array.isArray(result) ||
      typeof (result as Record<string, unknown>).archivedSessionId !== "string" ||
      !(result as Record<string, unknown>).archivedSessionId
    ) {
      throw new Error("ACP Rewind 响应缺少归档 Session 标识");
    }
    return result as SessionRewindResult;
  });
}

export async function sessionRename(args: {
  id: string;
  title: string;
  /** 标题 Journal 提交的业务幂等标识，放在 ACP 保留元数据中。 */
  operationId: string;
}): Promise<{
  /** 已持久化标题的会话标识。 */
  sessionId: string;
  /** 权威标题。 */
  title: string;
  /** 标题提交后的 Journal 水位。 */
  journalSequence: number;
}> {
  return acpRequest("keencode/session/rename", {
    sessionId: args.id,
    title: args.title,
    _meta: { "keencode/operationId": args.operationId },
  });
}

/** 切换会话级模型（Q1 决策：每会话独立 provider，不影响新会话默认值）。 */
export async function sessionSetModel(args: {
  sessionId: string;
  providerId: string;
  modelId: string;
  /** Provider 快照 Journal 提交的幂等标识。 */
  operationId: string;
}): Promise<void> {
  await acpRequest("session/set_config_option", {
    sessionId: args.sessionId, configId: "model",
    value: `${args.providerId}::${args.modelId}`,
    _meta: { "keencode/operationId": args.operationId },
  }, args.operationId);
}

/** 切换当前会话的推理强度，不影响其他会话或新会话默认值。 */
export async function sessionSetEffort(args: {
  sessionId: string;
  effort: string;
  /** 推理强度 Journal 提交的幂等标识。 */
  operationId: string;
}): Promise<void> {
  await acpRequest("session/set_config_option", {
    sessionId: args.sessionId, configId: "reasoning_effort", value: args.effort,
    _meta: { "keencode/operationId": args.operationId },
  }, args.operationId);
}

/** 使用当前 Session 供应商概括首条用户消息，返回后端已校验的标题候选。 */
export async function sessionGenerateTitle(args: {
  /** 目标 Session 标识。 */
  id: string;
  /** 首轮用户消息。 */
  userMessage: string;
  /** 标题模型调用与结果缓存共用的幂等标识。 */
  operationId: string;
}): Promise<string> {
  const result = await acpRequest<{ title: string }>("keencode/session/title", {
    sessionId: args.id,
    userMessage: args.userMessage,
    _meta: { "keencode/operationId": args.operationId },
  });
  if (typeof result.title !== "string" || !result.title.trim()) {
    throw new Error("ACP 标题响应缺少有效标题");
  }
  return result.title;
}

/** 永久删除一个已停止的根 Session 及其全部消息。 */
export async function sessionDelete(args: {
  /** 待删除的根 Session。 */
  id: string;
  /** 删除墓碑操作的幂等标识。 */
  operationId: string;
}): Promise<void> {
  await acpRequest("session/delete", { sessionId: args.id }, args.operationId);
}

export function sessionDisconnect(): Promise<SessionSnapshot> {
  return invoke<SessionSnapshot>("session_disconnect");
}

/** 标准 JSON-RPC 2.0 成功响应。 */
export interface AcpJsonRpcClientResponse {
  /** JSON-RPC 协议版本。 */
  jsonrpc: "2.0";
  /** 必须与 Client 请求完全相同的请求标识。 */
  id: AcpJsonRpcId;
  /** 接受表单并回传结构化内容，或取消本次问答。 */
  result:
    {
      /** 当前结构化问答动作。 */
      action: "accept" | "cancel";
      /** 接受表单时按 JSON Schema 字段返回的答案。 */
      content?: Record<string, unknown>;
    };
}

/** 把完整 ACP Client 响应原样交回 Runtime。 */
export function acpClientRespond(
  response: AcpJsonRpcClientResponse,
): Promise<void> {
  return acpRespond(response);
}

/** 构造 Prompt 停止时使用的标准取消响应。 */
export function cancelledClientResponse(
  requestId: AcpJsonRpcId,
): AcpJsonRpcClientResponse {
  return {
    jsonrpc: "2.0",
    id: requestId,
    result: { action: "cancel" },
  };
}

/** 构造接受结构化问答的标准响应。 */
export function acceptedElicitationResponse(
  requestId: AcpJsonRpcId,
  content: Record<string, unknown>,
): AcpJsonRpcClientResponse {
  return {
    jsonrpc: "2.0",
    id: requestId,
    result: { action: "accept", content },
  };
}

/** `keencode/goal/get` 的完整结果。 */
export interface GoalGetResult {
  /** 提供项目作用域的 Session 标识。 */
  sessionId: string;
  /** Goal 存储比较交换修订号。 */
  revision: number;
  /** 当前项目 Goal；修订号为零时字段缺失。 */
  goal?: GoalRecordDto;
}

/** 返回当前 Session 列表。 */
export async function sessionsList(): Promise<SessionListItem[]> {
  const sessions: SessionListItem[] = [];
  const cursors = new Set<string>();
  let cursor: string | undefined;
  do {
    const page = await acpRequest<{
      sessions: Array<{ sessionId: string; cwd: string; title?: string; updatedAt?: string }>;
      nextCursor?: string;
    }>("session/list", cursor === undefined ? {} : { cursor });
    if (!Array.isArray(page.sessions)) throw new Error("ACP Session 列表无效");
    for (const item of page.sessions) {
      if (typeof item.sessionId !== "string" || typeof item.cwd !== "string") {
        throw new Error("ACP Session 列表项无效");
      }
      sessions.push({ id: item.sessionId, cwd: item.cwd, title: item.title ?? null, updatedAt: item.updatedAt ?? "" });
    }
    cursor = page.nextCursor;
    if (cursor !== undefined) {
      if (typeof cursor !== "string" || !cursor || cursors.has(cursor)) {
        throw new Error("ACP Session 列表游标未推进");
      }
      cursors.add(cursor);
    }
  } while (cursor !== undefined);
  return sessions;
}

/** 从标准 Session 列表取得权威 cwd，不添加旧路径回退或另一套本地映射。 */
async function sessionCwd(sessionId: string): Promise<string> {
  const session = (await sessionsList()).find((item) => item.id === sessionId);
  if (!session) throw new Error("ACP Session 不存在或不在当前项目范围内");
  return session.cwd;
}

export function goalGet(sessionId: string): Promise<GoalGetResult> {
  return acpRequest<GoalGetResult>("keencode/goal/get", { sessionId });
}

/** 用户可以写入的 Goal 字段。 */
export interface GoalInputDto {
  /** Goal 用户可见标题。 */
  title: string;
  /** 完整目标描述。 */
  objective: string;
  /** 可选补充说明。 */
  description?: string;
  /** 可选人工进度百分比。 */
  progressPercent?: number;
  /** 可选 Token 预算。 */
  tokenBudget?: number;
}

export function goalUpsert(args: {
  /** 提供项目作用域的 Session 标识。 */
  sessionId: string;
  /** 当前唯一 Goal 的可编辑字段。 */
  goal: GoalInputDto;
  /** 比较交换修订号。 */
  expectedRevision: number;
  /** 本次变更的幂等标识。 */
  requestNonce: string;
}): Promise<{
  /** 提供项目作用域的 Session 标识。 */
  sessionId: string;
  /** 变更后的修订号。 */
  revision: number;
  /** 变更后的完整 Goal。 */
  goal: GoalRecordDto;
  /** 本次是否命中已完成的相同幂等请求。 */
  deduplicated: boolean;
}> {
  return acpRequest("keencode/goal/upsert", args, args.requestNonce);
}

export function goalTransition(args: {
  /** 提供项目作用域的 Session 标识。 */
  sessionId: string;
  /** 当前 Goal 标识。 */
  goalId: string;
  /** Goal 只能进入不可逆终态。 */
  status: "completed" | "blocked";
  /** 仅 blocked 状态必须携带的原因。 */
  reason?: string;
  /** 仅 completed 状态必须携带的非空验收证据。 */
  completionEvidence?: string;
  /** 比较交换修订号。 */
  expectedRevision: number;
  /** 本次变更的幂等标识。 */
  requestNonce: string;
}): Promise<{
  /** 提供项目作用域的 Session 标识。 */
  sessionId: string;
  /** 变更后的修订号。 */
  revision: number;
  /** 变更后的完整 Goal。 */
  goal: GoalRecordDto;
  /** 本次是否命中已完成的相同幂等请求。 */
  deduplicated: boolean;
}> {
  return acpRequest("keencode/goal/transition", args, args.requestNonce);
}

/** 清除当前 Session 的持久 Goal。 */
export function goalClear(args: {
  /** 提供项目作用域的 Session 标识。 */
  sessionId: string;
  /** 比较交换修订号。 */
  expectedRevision: number;
  /** 本次清理的幂等标识。 */
  requestNonce: string;
}): Promise<{
  /** 提供项目作用域的 Session 标识。 */
  sessionId: string;
  /** 清理后的修订号。 */
  revision: number;
  /** 已清理 Goal 的墓碑标识。 */
  clearedGoalId: string;
  /** 本次是否命中已完成的相同幂等请求。 */
  deduplicated: boolean;
}> {
  return acpRequest("keencode/goal/clear", args, args.requestNonce);
}

/** 标准 `session/load` 的控制响应；历史内容通过唯一投递通道恢复。 */
export interface SessionLoadResult {
  /** Runtime 当前模式能力。 */
  modes: {
    /** 当前模式，只允许默认执行或只读计划。 */
    currentModeId: "default" | "plan";
    /** Runtime 当前公布的完整模式目录。 */
    availableModes: Array<{
      /** 模式稳定标识。 */
      id: "default" | "plan";
      /** 模式用户可见名称。 */
      name: string;
    }>;
  };
  /** Runtime 当前 Session 配置项。 */
  configOptions: Array<{
    /** 配置项稳定标识。 */
    id: string;
    /** 配置项用户可见名称。 */
    name: string;
    /** 配置项说明。 */
    description?: string;
    /** 当前配置值。 */
    currentValue?: unknown;
    /** 可选值目录。 */
    options?: Array<Record<string, unknown>>;
    /** 标准配置项类别。 */
    type?: string;
  }>;
  /** ACP 标准透传元数据。 */
  _meta?: Record<string, unknown>;
}

/** 通过标准 ACP `session/load` 请求恢复 Session。 */
export async function sessionLoad(sessionId: string): Promise<SessionLoadResult> {
  return acpRequest<SessionLoadResult>("session/load", {
    sessionId, cwd: await sessionCwd(sessionId), mcpServers: [],
  });
}

/** `keencode/session/replay` 的分页控制响应。 */
export interface ReplayResult {
  /** 被重放的 Session。 */
  sessionId: string;
  /** 本页开始前的确认 Journal 水位。 */
  startAfter: number;
  /** 本页完成后的确认 Journal 水位。 */
  nextAfter: number;
  /** 本次读取观察到的 Journal 尾部。 */
  throughJournalSequence: number;
  /** 本页最后一条历史事件实际投递后的当前世代序号，不包含随后释放的实时事件。 */
  throughDeliverySequence: number;
  /** 本页实际重新投递的事件数。 */
  replayedEvents: number;
  /** 是否仍有下一页。 */
  hasMore: boolean;
}

/** 分页重放 KeenCode 权威 Journal；实际内容仍走 `acp://delivery`。 */
export function sessionReplay(args: {
  /** 目标 Session 标识。 */
  sessionId: string;
  /** 已确认 Journal 水位；首次从 load 快照水位开始时必须省略。 */
  after?: number;
  /** 单页事件数，范围 1..1000。 */
  limit: number;
}): Promise<ReplayResult> {
  if (args.after !== undefined && args.after <= 0) {
    return Promise.reject(new Error("replay after 必须为正整数或省略"));
  }
  if (!Number.isSafeInteger(args.limit) || args.limit < 1 || args.limit > 1_000) {
    return Promise.reject(new Error("replay limit 必须位于 1..1000"));
  }
  return acpRequest<ReplayResult>("keencode/session/replay", {
    sessionId: args.sessionId,
    ...(args.after === undefined ? {} : { after: args.after }),
    limit: args.limit,
  });
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
  return () => {
    try {
      unlisten();
    } catch {
      // Tauri 开发版热更新可能已先移除 listener；清理必须保持幂等。
    }
  };
}

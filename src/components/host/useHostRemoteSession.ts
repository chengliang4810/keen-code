import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  acceptedElicitationResponse,
  cancelledClientResponse,
  type SessionListItem,
} from "@/lib/acp/api";
import {
  parseAcpTauriDelivery,
  type SessionUpdateDeliveryEnvelope,
} from "@/lib/acp/events";
import { parseElicitationPayload } from "@/lib/elicitation";
import { buildRemotePrompt } from "@/lib/remotePrompt";
import type { AskUserPayload } from "@/lib/session";
import type { HostTransportAdapter } from "./hostMode";
import type { HostUploadedAttachment } from "./hostMode";
import type {
  MobileRemoteConnection,
  MobileRemoteActivity,
  MobileRemoteMessage,
  MobileRemoteSessionStatus,
} from "./MobileRemoteShell";

type JsonRecord = Record<string, unknown>;

interface RemoteSessionState {
  status: MobileRemoteSessionStatus;
  activeTurnId: string | null;
  messages: MobileRemoteMessage[];
  activities: MobileRemoteActivity[];
  lastUserText: string | null;
  lastUserTurnId: string | null;
}

function upsertActivity(
  activities: MobileRemoteActivity[],
  activity: MobileRemoteActivity,
): MobileRemoteActivity[] {
  const index = activities.findIndex((item) => item.id === activity.id);
  if (index < 0) return [...activities, activity];
  const next = activities.slice();
  next[index] = { ...next[index]!, ...activity };
  return next;
}

/** 把标准 ACP 工具内容投影为移动端只读状态和 Diff，不暴露终端引用。 */
export function reduceRemoteActivities(
  activities: MobileRemoteActivity[],
  envelope: SessionUpdateDeliveryEnvelope,
): MobileRemoteActivity[] {
  const update = envelope.update;
  if (update.sessionUpdate !== "tool_call" && update.sessionUpdate !== "tool_call_update") {
    return activities;
  }
  const current = activities.find((item) => item.id === `tool:${update.toolCallId}`);
  let next = upsertActivity(activities, {
    id: `tool:${update.toolCallId}`,
    kind: "tool",
    title: redactRemoteText(update.title ?? current?.title ?? "工具调用"),
    detail: null,
    status: update.status ?? current?.status ?? "in_progress",
  });
  for (const [index, content] of (update.content ?? []).entries()) {
    if (content.type !== "diff") continue;
    const oldText = content.oldText ?? "";
    const preview = `--- 修改前\n${oldText}\n+++ 修改后\n${content.newText}`;
    next = upsertActivity(next, {
      id: `diff:${update.toolCallId}:${index}`,
      kind: "diff",
      title: `文件变更：${redactRemoteText(content.path)}`,
      detail: preview.length > 12_000 ? `${preview.slice(0, 12_000)}\n…` : preview,
      status: update.status ?? "in_progress",
    });
  }
  return next;
}

const INITIALIZE_PARAMS: JsonRecord = {
  protocolVersion: 1,
  clientInfo: { name: "KeenCode Remote", version: "0.0.1" },
  clientCapabilities: { elicitation: { form: {} } },
};

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 只接受当前 Agent Session 的显式 `session/load` 恢复信号。 */
export function remoteGapRecoverySessionId(
  value: unknown,
  selectedSessionId: string | null,
): string | null {
  if (
    !selectedSessionId ||
    !isRecord(value) ||
    value.type !== "gap" ||
    value.snapshotRequired !== true ||
    value.recoveryMethod !== "session/load" ||
    value.sessionId !== selectedSessionId
  ) return null;
  return selectedSessionId;
}

function errorMessage(error: unknown): string {
  return redactRemoteText(
    error instanceof Error && error.message ? error.message : "远程请求失败。",
  );
}

/** 远程界面只展示文本投影，避免把本机绝对路径变成可操作的资源入口。 */
export function redactRemoteText(value: string): string {
  return value
    .replace(/\b[A-Za-z]:\\(?:[^\s<>:"|?*]+\\)*[^\s<>:"|?*]*/gu, "[本机路径]")
    .replace(/(^|[\s(])\/(?:Users|home|opt|var|tmp|etc)\/(?:[^\s)]+\/?)+/gmu, "$1[本机路径]");
}

function textContent(update: SessionUpdateDeliveryEnvelope["update"]): string | null {
  if (
    update.sessionUpdate !== "user_message_chunk" &&
    update.sessionUpdate !== "agent_message_chunk"
  ) return null;
  return update.content.type === "text" ? redactRemoteText(update.content.text) : null;
}

function rawUserText(update: SessionUpdateDeliveryEnvelope["update"]): string | null {
  return update.sessionUpdate === "user_message_chunk" && update.content.type === "text"
    ? update.content.text
    : null;
}

function sanitizeAskPayload(payload: AskUserPayload): AskUserPayload {
  return {
    ...payload,
    questions: payload.questions.map((question) => ({
      ...question,
      question: redactRemoteText(question.question),
      options: question.options.map((option) => ({
        ...option,
        label: redactRemoteText(option.label),
        ...(option.description
          ? { description: redactRemoteText(option.description) }
          : {}),
      })),
    })),
  };
}

function messageKey(envelope: SessionUpdateDeliveryEnvelope): string {
  const role = envelope.update.sessionUpdate === "user_message_chunk" ? "user" : "assistant";
  const meta = envelope.update._meta;
  const messageId = meta?.["keencode/messageId"];
  return `${role}:${envelope.turnId ?? (typeof messageId === "string" ? messageId : envelope.deliverySequence)}`;
}

/** 将一条严格 ACP 文本投递并入手机端最小消息投影。 */
export function reduceRemoteMessages(
  messages: MobileRemoteMessage[],
  envelope: SessionUpdateDeliveryEnvelope,
): MobileRemoteMessage[] {
  const text = textContent(envelope.update);
  if (text === null || (envelope.sourceAgentId && envelope.sourceAgentId !== "root")) {
    return messages;
  }
  const role = envelope.update.sessionUpdate === "user_message_chunk" ? "user" : "assistant";
  const id = messageKey(envelope);
  const index = messages.findIndex((message) => message.id === id);
  if (index < 0) {
    return [...messages, {
      id,
      role,
      text,
      occurredAtMs: envelope.occurredAtMs,
      turnId: envelope.turnId ?? null,
    }];
  }
  const next = [...messages];
  next[index] = { ...next[index]!, text: `${next[index]!.text}${text}` };
  return next;
}

async function rpcRequest<T>(
  transport: HostTransportAdapter,
  method: string,
  params: JsonRecord,
  requestId = `remote-${globalThis.crypto.randomUUID()}`,
): Promise<T> {
  if (!transport.dispatch) throw new Error("远程 Host transport 不支持 ACP 请求。");
  const response = await transport.dispatch({ jsonrpc: "2.0", id: requestId, method, params });
  if (!isRecord(response) || response.id !== requestId) {
    throw new Error("远程 Host 返回了无效的 ACP 响应。");
  }
  if (Object.hasOwn(response, "error")) {
    const error = isRecord(response.error) && typeof response.error.message === "string"
      ? response.error.message
      : "远程 Host 拒绝了请求。";
    throw new Error(error);
  }
  if (!Object.hasOwn(response, "result")) throw new Error("远程 Host 响应缺少结果。");
  return response.result as T;
}

async function rpcNotify(
  transport: HostTransportAdapter,
  method: string,
  params: JsonRecord,
): Promise<void> {
  if (!transport.dispatch) throw new Error("远程 Host transport 不支持 ACP 通知。");
  await transport.dispatch({ jsonrpc: "2.0", method, params });
}

async function listRemoteSessions(transport: HostTransportAdapter): Promise<SessionListItem[]> {
  const sessions: SessionListItem[] = [];
  const cursors = new Set<string>();
  let cursor: string | undefined;
  do {
    const page = await rpcRequest<{
      sessions: Array<{
        sessionId: string;
        cwd: string;
        title?: string;
        updatedAt?: string;
        _meta?: JsonRecord;
      }>;
      nextCursor?: string;
    }>(transport, "session/list", cursor ? { cursor } : {});
    if (!Array.isArray(page.sessions)) throw new Error("远程 Session 列表无效。");
    for (const item of page.sessions) {
      if (!item || typeof item.sessionId !== "string" || typeof item.cwd !== "string") {
        throw new Error("远程 Session 列表项无效。");
      }
      const lastUserMessageAt = item._meta?.["keencode/lastUserMessageAt"];
      sessions.push({
        id: item.sessionId,
        title: typeof item.title === "string" ? item.title : null,
        cwd: item.cwd,
        updatedAt: typeof item.updatedAt === "string" ? item.updatedAt : "",
        lastUserMessageAt: typeof lastUserMessageAt === "string" ? lastUserMessageAt : null,
      });
    }
    cursor = page.nextCursor;
    if (cursor !== undefined) {
      if (!cursor || cursors.has(cursor)) throw new Error("远程 Session 列表游标未推进。");
      cursors.add(cursor);
    }
  } while (cursor !== undefined);
  return sessions;
}

function emptySessionState(): RemoteSessionState {
  return {
    status: "idle",
    activeTurnId: null,
    messages: [],
    activities: [],
    lastUserText: null,
    lastUserTurnId: null,
  };
}

/** 保留远程壳层的公开导出，完整工作台与移动端共用同一 Prompt 语义。 */
export { buildRemotePrompt } from "@/lib/remotePrompt";

export type RemoteDeliverySequenceDecision = "accept" | "duplicate" | "recover" | "stale";

/** 连接级缺口冻结当前增量；恢复期间只接受从 1 开始的新投递世代。 */
export function remoteDeliverySequenceDecision(
  previous: number | undefined,
  incoming: number,
  recovering: boolean,
): RemoteDeliverySequenceDecision {
  if (previous !== undefined && incoming <= previous) return "duplicate";
  const expected = previous === undefined ? 1 : previous + 1;
  if (incoming === expected) return "accept";
  return recovering ? "stale" : "recover";
}

export function useHostRemoteSession(transport: HostTransportAdapter | null) {
  const [connection, setConnection] = useState<MobileRemoteConnection>(
    transport ? "connecting" : "unauthorized",
  );
  const [sessions, setSessions] = useState<SessionListItem[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [sessionStates, setSessionStates] = useState<Record<string, RemoteSessionState>>({});
  const [pendingAsk, setPendingAsk] = useState<AskUserPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [restoring, setRestoring] = useState(false);
  const [sending, setSending] = useState(false);
  const selectedRef = useRef<string | null>(null);
  const sessionsRef = useRef<SessionListItem[]>([]);
  const deliverySequenceRef = useRef(new Map<string, number>());
  const recoveringRef = useRef(new Set<string>());
  const recoveryRef = useRef<(sessionId: string) => Promise<void>>(async () => {});

  selectedRef.current = selectedSessionId;
  sessionsRef.current = sessions;

  const updateSession = useCallback((sessionId: string, update: (state: RemoteSessionState) => RemoteSessionState) => {
    setSessionStates((previous) => ({
      ...previous,
      [sessionId]: update(previous[sessionId] ?? emptySessionState()),
    }));
  }, []);

  const loadSession = useCallback(async (sessionId: string, recovery = false) => {
    if (!transport) return;
    const session = sessionsRef.current.find((item) => item.id === sessionId);
    if (!session) throw new Error("远程 Session 不存在。");
    // `session/load` 会立即触发历史回放；先同步引用，避免首批事件在
    // React 状态更新尚未提交时被旧的 selectedSessionId 过滤掉。
    selectedRef.current = sessionId;
    if (recovery) recoveringRef.current.add(sessionId);
    deliverySequenceRef.current.delete(sessionId);
    setRestoring(true);
    setError(null);
    setPendingAsk((current) => current?.sessionId === sessionId ? null : current);
    updateSession(sessionId, () => emptySessionState());
    try {
      await rpcRequest(transport, "session/load", {
        sessionId,
        cwd: session.cwd,
        mcpServers: [],
        _meta: { "keencode/history": { limit: recovery ? -1 : 100 } },
      });
    } finally {
      recoveringRef.current.delete(sessionId);
      setRestoring(false);
    }
  }, [transport, updateSession]);
  recoveryRef.current = (sessionId) => loadSession(sessionId, true);

  const handleDelivery = useCallback((raw: unknown) => {
    const recoverySessionId = remoteGapRecoverySessionId(raw, selectedRef.current);
    if (recoverySessionId) {
      if (!recoveringRef.current.has(recoverySessionId)) {
        void recoveryRef.current(recoverySessionId).catch((cause) => setError(errorMessage(cause)));
      }
      return;
    }
    const delivery = parseAcpTauriDelivery(raw);
    if (!delivery) return;
    if (delivery.type === "client_request") {
      const payload = parseElicitationPayload(delivery.request);
      if (!payload) {
        if (transport?.dispatch) void transport.dispatch(cancelledClientResponse(delivery.request.id));
        return;
      }
      setPendingAsk((current) => {
        if (current && current.sessionId === payload.sessionId && current.rpcId !== payload.rpcId) {
          if (transport?.dispatch) void transport.dispatch(cancelledClientResponse(payload.rpcId));
          return current;
        }
        return sanitizeAskPayload(payload);
      });
      return;
    }
    if (delivery.type === "notification") return;
    const envelope = delivery.envelope;
    if (envelope.sessionId !== selectedRef.current) return;
    const previousSequence = deliverySequenceRef.current.get(envelope.sessionId);
    const sequenceDecision = remoteDeliverySequenceDecision(
      previousSequence,
      envelope.deliverySequence,
      recoveringRef.current.has(envelope.sessionId),
    );
    if (sequenceDecision !== "accept") {
      if (sequenceDecision === "recover") {
        void recoveryRef.current(envelope.sessionId).catch((cause) => setError(errorMessage(cause)));
      }
      return;
    }
    deliverySequenceRef.current.set(envelope.sessionId, envelope.deliverySequence);
    if (delivery.type === "session_update") {
      const updateEnvelope = delivery.envelope;
      updateSession(envelope.sessionId, (state) => {
        const messages = reduceRemoteMessages(state.messages, updateEnvelope);
        const activities = reduceRemoteActivities(state.activities, updateEnvelope);
        const rawUser = rawUserText(updateEnvelope.update);
        return {
          ...state,
          messages,
          activities,
          lastUserText: rawUser === null
            ? state.lastUserText
            : updateEnvelope.turnId && updateEnvelope.turnId === state.lastUserTurnId
              ? `${state.lastUserText ?? ""}${rawUser}`
              : rawUser,
          lastUserTurnId: rawUser === null
            ? state.lastUserTurnId
            : updateEnvelope.turnId ?? null,
        };
      });
      return;
    }
    const event = delivery.envelope.event;
    updateSession(envelope.sessionId, (state) => {
      if (event.type === "agent_spawned") {
        return {
          ...state,
          activities: upsertActivity(state.activities, {
            id: `agent:${event.agentId}`,
            kind: "agent",
            title: redactRemoteText(event.agentPath),
            detail: redactRemoteText(event.task),
            status: "running",
          }),
        };
      }
      if (event.type === "agent_status_changed") {
        const current = state.activities.find((item) => item.id === `agent:${event.agentId}`);
        return {
          ...state,
          activities: upsertActivity(state.activities, {
            id: `agent:${event.agentId}`,
            kind: "agent",
            title: current?.title ?? "子 Agent",
            detail: current?.detail,
            status: event.status,
          }),
        };
      }
      if (event.type === "turn_started") {
        return { ...state, status: "running", activeTurnId: delivery.envelope.turnId ?? null };
      }
      if (event.type === "turn_failed") {
        return { ...state, status: "failed", activeTurnId: null };
      }
      if (event.type === "turn_completed" || event.type === "turn_cancelled") {
        return { ...state, status: "idle", activeTurnId: null };
      }
      return state;
    });
  }, [transport, updateSession]);

  useEffect(() => {
    if (!transport) return;
    let disposed = false;
    let unsubscribeDelivery: (() => void) | null = null;
    let unsubscribeSnapshot: (() => void) | null = null;
    const start = async () => {
      try {
        const current = await transport.snapshot?.();
        if (disposed) return;
        if (current) setConnection(current.connection);
        if (transport.subscribe) {
          unsubscribeSnapshot = await transport.subscribe((snapshot) => {
            if (!disposed) setConnection(snapshot.connection);
          });
        }
        if (transport.subscribeDelivery) {
          unsubscribeDelivery = await transport.subscribeDelivery(handleDelivery);
        }
        await rpcRequest(transport, "initialize", INITIALIZE_PARAMS);
        if (disposed) return;
        setConnection("connected");
        const nextSessions = await listRemoteSessions(transport);
        if (disposed) return;
        sessionsRef.current = nextSessions;
        setSessions(nextSessions);
        const first = selectedRef.current ?? nextSessions[0]?.id ?? null;
        setSelectedSessionId(first);
        if (first) await loadSession(first);
      } catch (cause) {
        if (!disposed) {
          setConnection("offline");
          setError(errorMessage(cause));
        }
      }
    };
    void start();
    return () => {
      disposed = true;
      unsubscribeDelivery?.();
      unsubscribeSnapshot?.();
    };
  }, [handleDelivery, loadSession, transport]);

  const selectSession = useCallback(async (sessionId: string) => {
    selectedRef.current = sessionId;
    setSelectedSessionId(sessionId);
    await loadSession(sessionId);
  }, [loadSession]);

  const send = useCallback(async (text: string, attachments: HostUploadedAttachment[] = []) => {
    const sessionId = selectedRef.current;
    const value = text.trim();
    if (!transport || !sessionId || (!value && attachments.length === 0) || sending) return;
    const requestId = `remote-turn-${globalThis.crypto.randomUUID()}`;
    setSending(true);
    setError(null);
    updateSession(sessionId, (state) => ({ ...state, status: "running", activeTurnId: requestId }));
    try {
      await rpcRequest(transport, "session/set_mode", { sessionId, modeId: "default" });
      await rpcRequest(transport, "session/prompt", {
        sessionId,
        prompt: buildRemotePrompt(value, attachments),
        _meta: { "keencode/turnId": requestId, "keencode/ultraMode": false },
      }, requestId);
    } catch (cause) {
      updateSession(sessionId, (state) => ({ ...state, status: "failed", activeTurnId: null }));
      setError(errorMessage(cause));
    } finally {
      setSending(false);
    }
  }, [sending, transport, updateSession]);

  const uploadAttachment = useCallback(async (file: File) => {
    if (!transport?.uploadAttachment) throw new Error("当前 Web Host 不支持附件上传。");
    return transport.uploadAttachment(file);
  }, [transport]);

  const stop = useCallback(async () => {
    const sessionId = selectedRef.current;
    const state = sessionId ? sessionStates[sessionId] : undefined;
    if (!transport || !sessionId || !state?.activeTurnId) return;
    await rpcNotify(transport, "session/cancel", {
      sessionId,
      _meta: { "keencode/turnId": state.activeTurnId },
    });
  }, [sessionStates, transport]);

  const retry = useCallback(async () => {
    const sessionId = selectedRef.current;
    const text = sessionId ? sessionStates[sessionId]?.lastUserText : null;
    if (text) await send(text);
  }, [send, sessionStates]);

  const answer = useCallback(async (content: Record<string, string | string[]>) => {
    if (!transport?.dispatch || !pendingAsk) return;
    await transport.dispatch(acceptedElicitationResponse(pendingAsk.rpcId, content));
    setPendingAsk(null);
  }, [pendingAsk, transport]);

  const cancelAnswer = useCallback(async () => {
    if (!transport?.dispatch || !pendingAsk) return;
    await transport.dispatch(cancelledClientResponse(pendingAsk.rpcId));
    setPendingAsk(null);
  }, [pendingAsk, transport]);

  const reconnect = useCallback(async () => {
    if (!transport?.reconnect) return;
    setConnection("reconnecting");
    setError(null);
    try {
      await transport.reconnect();
      setConnection("connected");
      const nextSessions = await listRemoteSessions(transport);
      sessionsRef.current = nextSessions;
      setSessions(nextSessions);
      const sessionId = selectedRef.current && nextSessions.some((item) => item.id === selectedRef.current)
        ? selectedRef.current
        : nextSessions[0]?.id ?? null;
      setSelectedSessionId(sessionId);
      if (sessionId) await loadSession(sessionId, true);
    } catch (cause) {
      setConnection("offline");
      setError(errorMessage(cause));
    }
  }, [loadSession, transport]);

  const current = selectedSessionId ? sessionStates[selectedSessionId] ?? emptySessionState() : null;
  return useMemo(() => ({
    connection,
    sessions: sessions.map(({ cwd: _cwd, ...item }) => ({
      ...item,
      title: item.title ? redactRemoteText(item.title) : null,
    })),
    selectedSessionId,
    current,
    pendingAsk: pendingAsk?.sessionId === selectedSessionId ? pendingAsk : null,
    error,
    restoring,
    sending,
    selectSession,
    send,
    uploadAttachment,
    stop,
    retry,
    reconnect,
    answer,
    cancelAnswer,
  }), [
    answer, cancelAnswer, connection, current, error, pendingAsk, reconnect, restoring,
    retry, selectSession, selectedSessionId, send, sending, sessions, stop, uploadAttachment,
  ]);
}

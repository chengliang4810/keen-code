import { useEffect } from "react";
import * as api from "@/lib/api";
import type {
  AskUserPayload,
  ChatMessage,
  SessionSnapshot,
} from "@/lib/session";
import type { SessionContextUsage } from "@/features/app/models";
import {
  acpClientRespond,
  cancelledClientResponse,
  diagnosticsRecord,
  goalGet,
  listenAcp,
} from "@/lib/acp/api";
import { parseElicitationPayload } from "@/lib/elicitation";
import { ensureAcpSession } from "@/lib/acp/projection";
import {
  reduceDeliveryEnvelope,
  reduceGoalSnapshot,
  type AcpDeliveryReduction,
  type AcpSessionView,
  type AcpWorkspaceState,
} from "@/lib/acp/store";
import {
  isTerminalKeenCodeEvent,
  parseAcpTauriDelivery,
  shouldDriveMainSessionStreaming,
  type AcpJsonRpcClientRequest,
  type KeenCodeEventEnvelope,
  type McpOAuthEvent,
  type SessionUpdateDeliveryEnvelope,
} from "@/lib/acp/events";
import {
  projectHostIntoLiveMap,
  type SessionLiveMap,
} from "@/lib/sessionLiveStore";
import {
  reduceTurnLatency,
  summarizeTurnLatency,
  turnLatencyNow,
  type TurnLatencyState,
} from "@/lib/turnLatency";
import { saveCompletedUnreadSessionIds } from "@/lib/sessionCompletion";
import { createAnimationFrameBatcher } from "@/lib/frameBatcher";
import type { Ref, SetState, ViewProjection } from "./types";

/** 前端内部转发已经严格解析的 KeenCode 生命周期事件。 */
export const KEENCODE_ACP_EVENT = "keencode:acp-event";

/** 前端内部转发已经严格解析的项目级 MCP OAuth 通知。 */
export const KEENCODE_MCP_OAUTH_EVENT = "keencode:mcp-oauth";

/** ACP Runtime 事件 Hook 的全部状态依赖。 */
export interface AcpRuntimeEventsOptions {
  /** 发布 ACP 工作区引用中的变更。 */
  commitWorkspace: () => void;
  /** 当前 ACP UI 投影。 */
  acpWorkspaceRef: Ref<AcpWorkspaceState>;
  /** 每个 Session 的本轮延迟观测。 */
  turnLatencyBySessionRef: Ref<Map<string, TurnLatencyState>>;
  /** 每个 Session 当前根 Turn。 */
  activeTurnIdBySessionRef: Ref<Map<string, string>>;
  /** 旧恢复窗口留下的完成 Turn 关联；新事件会主动清理。 */
  recoverableCompletedTurnIdBySessionRef: Ref<Map<string, string>>;
  /** 每个 Session 最近完成的根 Turn。 */
  completedTurnIdBySessionRef: Ref<Map<string, string>>;
  /** 等待浏览器提交首个可见 Token 的 Turn。 */
  pendingVisibleTurnBySessionRef: Ref<Map<string, string>>;
  /** 当前 Host 快照。 */
  liveHostRef: Ref<SessionSnapshot>;
  /** 每个 Session 当前界面消息，用于补齐本地乐观用户消息。 */
  messagesBySessionRef: Ref<Map<string, ChatMessage[]>>;
  /** 每个 Session 当前模型。 */
  modelBySessionRef: Ref<Map<string, string>>;
  /** 每个 Session 标准上下文占用。 */
  contextUsageBySessionRef: Ref<Map<string, SessionContextUsage>>;
  /** 当前界面正在查看的 Session。 */
  viewingSessionIdRef: Ref<string | null>;
  /** 当前仍可选择的模型目录。 */
  configuredModelsRef: Ref<Array<{ id: string }>>;
  /** 移除已经响应或取消的 Client 请求卡片。 */
  clearPendingAskUserRef: Ref<
    (sessionId?: string | null, rpcId?: string | number) => void
  >;
  /** 每个 Session 当前唯一展示中的 Client 请求。 */
  pendingAskUserBySessionRef: Ref<Map<string, AskUserPayload>>;
  /** 更新侧栏的待输入 Session 集合。 */
  setPendingAskUserSessionIds: SetState<Set<string>>;
  /** 更新当前会话的 Client 请求卡片。 */
  setAskUser: SetState<AskUserPayload | null>;
  /** 更新当前会话上下文占用。 */
  setContextUsage: SetState<SessionContextUsage | null>;
  /** 更新当前 Host 快照。 */
  setLiveHost: SetState<SessionSnapshot>;
  /** 更新全部 Session 的忙闲投影。 */
  setLiveMap: SetState<SessionLiveMap>;
  /** 更新当前根 Turn 起始时间。 */
  setTurnStartedAt: SetState<number | null>;
  /** 更新当前会话模型选择。 */
  setModelId: SetState<string>;
  /** 更新已完成未读 Session 集合。 */
  setCompletedUnreadIds: SetState<Set<string>>;
  /** 把指定 Session 投影到界面的稳定引用。 */
  applyViewProjectionRef: Ref<ViewProjection>;
  /** 刷新当前 Session 的本地缓存用量。 */
  refreshTaskCacheUsage: (sessionId: string | null) => Promise<void>;
  /** 投递缺口后的标准 load/replay 恢复入口。 */
  recoverSession: (sessionId: string) => Promise<void>;
  /** 共享 Reducer 已处理投递，通知恢复流程核对真实消费水位。 */
  observeSessionDelivery: (sessionId: string) => void;
}

/** 判断未知值是否为普通对象。 */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 仅让已经通过共享投递水位门禁的 Runtime 重放信号启动一次标准恢复。 */
export function shouldRecoverFromRuntimeReplay(
  envelope: KeenCodeEventEnvelope,
  reduction: AcpDeliveryReduction,
): boolean {
  return reduction.status === "applied" &&
    envelope.event.type === "recovery_state_changed" &&
    envelope.event.state === "replaying";
}

/**
 * 只以当前前端的同一单调时钟记录已接受的根 Turn 信号。
 * 远端/宿主墙钟仅用于日志排序；正文到达与 DOM 首次显示是两个独立观测。
 */
export function observeTurnLatencyDelivery(
  state: TurnLatencyState,
  envelope: SessionUpdateDeliveryEnvelope | KeenCodeEventEnvelope,
  reduction: AcpDeliveryReduction,
  wasRecovering: boolean,
  receivedAtMs: number,
): TurnLatencyState {
  if (wasRecovering || reduction.status !== "applied" ||
    reduction.childAgentId || reduction.ignoredTerminalUpdate ||
    envelope.turnId !== state.turnId || state.completedAtMs != null) return state;
  if ("update" in envelope) {
    const update = envelope.update;
    if ((update.sessionUpdate === "agent_message_chunk" ||
      update.sessionUpdate === "agent_thought_chunk") &&
      update.content.type === "text" && update.content.text.length > 0) {
      return reduceTurnLatency(state, {
        type: "first_token", turnId: state.turnId, atMs: receivedAtMs,
      });
    }
    return state;
  }
  const event = envelope.event;
  if (event.type === "turn_started" && event.parentTurnId === undefined) {
    return reduceTurnLatency(state, {
      type: "send_acknowledged", turnId: state.turnId, atMs: receivedAtMs,
    });
  }
  if (event.type === "model_first_stream_observed") {
    return reduceTurnLatency(state, {
      type: "first_sse", turnId: state.turnId, atMs: receivedAtMs,
    });
  }
  return isTerminalKeenCodeEvent(event)
    ? reduceTurnLatency(state, {
        type: "completed", turnId: state.turnId, atMs: receivedAtMs,
      })
    : state;
}

/** 在终态归约前补入尚未由 Runtime 回放的本地乐观用户消息。 */
function appendOptimisticUser(
  view: AcpSessionView,
  messages: readonly ChatMessage[],
): ChatMessage | undefined {
  const optimistic = messages
    .slice()
    .reverse()
    .find((message) => message.role === "user" && message.id.startsWith("u-"));
  if (!optimistic) return undefined;
  const last = view.history.at(-1);
  if (last?.role !== "user" || last.content !== optimistic.content) {
    view.history.push({ role: "user", content: optimistic.content });
  }
  return optimistic;
}

/** 将终态延迟和模型信息补写到统一 Reducer 已提交的 Assistant Turn。 */
function patchCompletedTurn(
  view: AcpSessionView,
  turnId: string,
  latency: TurnLatencyState | null,
  model: string | undefined,
): void {
  const message = view.history
    .slice()
    .reverse()
    .find((item) => item.role === "assistant" && item.turnId === turnId);
  if (!message) return;
  if (latency) message.turnMetrics = summarizeTurnLatency(latency);
  if (model) message.model = model;
}

/**
 * 订阅唯一 `acp://delivery`，按共享水位归约并在绘制边界发布当前 Session。
 */
export function useAcpRuntimeEvents({
  commitWorkspace,
  acpWorkspaceRef,
  turnLatencyBySessionRef,
  activeTurnIdBySessionRef,
  recoverableCompletedTurnIdBySessionRef,
  completedTurnIdBySessionRef,
  pendingVisibleTurnBySessionRef,
  liveHostRef,
  messagesBySessionRef,
  modelBySessionRef,
  contextUsageBySessionRef,
  viewingSessionIdRef,
  configuredModelsRef,
  clearPendingAskUserRef,
  pendingAskUserBySessionRef,
  setPendingAskUserSessionIds,
  setAskUser,
  setContextUsage,
  setLiveHost,
  setLiveMap,
  setTurnStartedAt,
  setModelId,
  setCompletedUnreadIds,
  applyViewProjectionRef,
  refreshTaskCacheUsage,
  recoverSession,
  observeSessionDelivery,
}: AcpRuntimeEventsOptions): void {
  useEffect(() => {
    if (!api.isTauri()) return;
    let disposed = false;
    let unlisten: (() => void) | null = null;
    const pendingProjectionSessions = new Set<string>();
    const publishScheduled = () => {
      if (disposed) return;
      const viewingSessionId = viewingSessionIdRef.current;
      const projectViewing = viewingSessionId !== null &&
        pendingProjectionSessions.has(viewingSessionId);
      pendingProjectionSessions.clear();
      commitWorkspace();
      if (projectViewing) applyViewProjectionRef.current(viewingSessionId);
    };
    const projectionBatcher = createAnimationFrameBatcher(
      publishScheduled,
      (callback) => window.setTimeout(() => callback(performance.now()), 100),
      (id) => window.clearTimeout(id),
    );
    const scheduleProjection = (sessionId: string) => {
      pendingProjectionSessions.add(sessionId);
      projectionBatcher.schedule();
    };
    const flushProjection = (sessionId: string) => {
      pendingProjectionSessions.add(sessionId);
      if (viewingSessionIdRef.current === sessionId) projectionBatcher.flush();
      else projectionBatcher.schedule();
    };
    const updateHostState = (
      sessionId: string,
      state: "ready" | "streaming",
    ) => {
      const projectedState = acpWorkspaceRef.current.sessions[sessionId]?.replay.restoring
        ? "connecting" : state;
      setLiveMap((previous) => projectHostIntoLiveMap(previous, {
        sessionId,
        state: projectedState,
        streamingMessageId: null,
      }));
      setLiveHost((previous) => {
        if (previous.sessionId !== sessionId) return previous;
        const next: SessionSnapshot = { ...previous, state: projectedState, streamingMessageId: null };
        liveHostRef.current = next;
        return next;
      });
    };
    const cancelClientRequest = (request: AcpJsonRpcClientRequest) => {
      void acpClientRespond(cancelledClientResponse(request.id))
        .catch(() => {});
    };
    const showClientRequest = (request: AcpJsonRpcClientRequest) => {
      const payload = parseElicitationPayload(request);
      if (!payload) {
        cancelClientRequest(request);
        return;
      }
      const pending = pendingAskUserBySessionRef.current.get(payload.sessionId);
      if (pending && pending.rpcId !== payload.rpcId) {
        cancelClientRequest(request);
        void diagnosticsRecord(
          "frontend.acp_client_request",
          "同一 Session 收到并发 Client 请求，已取消非队首请求",
        ).catch(() => {});
        return;
      }
      pendingAskUserBySessionRef.current.set(payload.sessionId, payload);
      setPendingAskUserSessionIds((previous) => {
        if (previous.has(payload.sessionId)) return previous;
        const next = new Set(previous);
        next.add(payload.sessionId);
        return next;
      });
      if (viewingSessionIdRef.current === payload.sessionId) setAskUser(payload);
    };
    const recoverGap = (
      sessionId: string,
      reduction: Extract<AcpDeliveryReduction, { status: "gap" }>,
    ) => {
      void diagnosticsRecord(
        "frontend.acp_delivery_gap",
        `Session 投递缺口：期望 ${reduction.expectedSequence}，收到 ${reduction.receivedSequence}`,
      ).catch(() => {});
      void recoverSession(sessionId).catch((error) => {
        void diagnosticsRecord(
          "frontend.acp_recovery",
          error instanceof Error ? error.message : String(error),
        ).catch(() => {});
      });
    };
    const applyUsageAndConfig = (
      envelope: SessionUpdateDeliveryEnvelope,
      childAgentId: string | undefined,
    ) => {
      const update = envelope.update;
      if (!childAgentId && update.sessionUpdate === "usage_update") {
        const usage: SessionContextUsage = {
          used: update.used,
          size: update.size,
          estimated: false,
        };
        contextUsageBySessionRef.current.set(envelope.sessionId, usage);
        if (viewingSessionIdRef.current === envelope.sessionId) {
          setContextUsage(usage);
        }
      }
      if (update.sessionUpdate === "config_option_update") {
        const modelOption = update.configOptions.find(
          (option) => option.id === "model",
        );
        const modelValue = modelOption?.currentValue;
        if (typeof modelValue === "string" && modelValue.length > 0) {
          modelBySessionRef.current.set(envelope.sessionId, modelValue);
          if (viewingSessionIdRef.current === envelope.sessionId &&
            configuredModelsRef.current.some((model) => model.id === modelValue)) {
            setModelId(modelValue);
          }
        }
      }
    };
    const applyTerminalSideEffects = (
      envelope: KeenCodeEventEnvelope,
      view: AcpSessionView,
      wasRecovering: boolean,
      hadVisibleMainText: boolean,
      optimisticUser: ChatMessage | undefined,
    ) => {
      const turnId = envelope.turnId;
      if (!turnId) return;
      completedTurnIdBySessionRef.current.set(envelope.sessionId, turnId);
      activeTurnIdBySessionRef.current.delete(envelope.sessionId);
      recoverableCompletedTurnIdBySessionRef.current.delete(envelope.sessionId);
      const activeLatency = turnLatencyBySessionRef.current.get(envelope.sessionId);
      const completedLatency = activeLatency?.turnId === turnId
        ? activeLatency : null;
      patchCompletedTurn(
        view,
        turnId,
        completedLatency,
        optimisticUser?.model ?? modelBySessionRef.current.get(envelope.sessionId),
      );
      if (completedLatency && !wasRecovering && !completedLatency.deliveryInterrupted &&
        completedLatency.completedAtMs != null && completedLatency.firstVisibleTokenAtMs === null &&
        hadVisibleMainText && viewingSessionIdRef.current === envelope.sessionId) {
        pendingVisibleTurnBySessionRef.current.set(envelope.sessionId, turnId);
        turnLatencyBySessionRef.current.set(envelope.sessionId, completedLatency);
      } else {
        // 历史旧 Turn 的终态不得清理同一 Session 当前 Turn 的实时观测。
        if (pendingVisibleTurnBySessionRef.current.get(envelope.sessionId) === turnId) {
          pendingVisibleTurnBySessionRef.current.delete(envelope.sessionId);
        }
        if (completedLatency) turnLatencyBySessionRef.current.delete(envelope.sessionId);
      }
      if (!wasRecovering && envelope.event.type === "turn_completed" &&
        viewingSessionIdRef.current !== envelope.sessionId) {
        setCompletedUnreadIds((previous) => {
          if (previous.has(envelope.sessionId)) return previous;
          const next = new Set(previous);
          next.add(envelope.sessionId);
          saveCompletedUnreadSessionIds(next, localStorage);
          return next;
        });
      }
      updateHostState(envelope.sessionId, "ready");
      if (viewingSessionIdRef.current === envelope.sessionId) {
        setTurnStartedAt(null);
        if (!wasRecovering) void refreshTaskCacheUsage(envelope.sessionId);
      }
      const pending = pendingAskUserBySessionRef.current.get(envelope.sessionId);
      if (pending) {
        void acpClientRespond(cancelledClientResponse(pending.rpcId))
          .catch(() => {});
      }
      clearPendingAskUserRef.current(envelope.sessionId);
      setAskUser((current) =>
        current?.sessionId === envelope.sessionId ? null : current);
    };
    const handleKeenCodeEvent = (
      envelope: KeenCodeEventEnvelope,
      reduction: Extract<AcpDeliveryReduction, { status: "applied" }>,
      view: AcpSessionView,
      wasRecovering: boolean,
      hadVisibleMainText: boolean,
      optimisticUser: ChatMessage | undefined,
      terminalRoot: boolean,
    ) => {
      window.dispatchEvent(new CustomEvent<KeenCodeEventEnvelope>(
        KEENCODE_ACP_EVENT,
        { detail: envelope },
      ));
      const event = envelope.event;
      if (event.type === "turn_started" && !reduction.childAgentId &&
        event.parentTurnId === undefined && envelope.turnId) {
        activeTurnIdBySessionRef.current.set(envelope.sessionId, envelope.turnId);
        completedTurnIdBySessionRef.current.delete(envelope.sessionId);
        recoverableCompletedTurnIdBySessionRef.current.delete(envelope.sessionId);
        updateHostState(envelope.sessionId, "streaming");
        if (viewingSessionIdRef.current === envelope.sessionId) {
          setTurnStartedAt(envelope.occurredAtMs);
        }
      }
      // 只有本次真正结束活跃根 Turn 的信封才清理 Host、问答及计时状态。
      if (terminalRoot && !reduction.childAgentId) {
        applyTerminalSideEffects(
          envelope,
          view,
          wasRecovering,
          hadVisibleMainText,
          optimisticUser,
        );
      }
      if (event.type === "goal_changed") {
        void goalGet(envelope.sessionId).then((result) => {
          if (disposed) return;
          const current = acpWorkspaceRef.current.sessions[envelope.sessionId];
          if (!current || result.revision < current.goal.revision) return;
          reduceGoalSnapshot(current, result.revision, result.goal ?? null);
          flushProjection(envelope.sessionId);
        }).catch(() => {});
      }
      if (shouldRecoverFromRuntimeReplay(envelope, reduction)) {
        void recoverSession(envelope.sessionId).catch((error) => {
          void diagnosticsRecord(
            "frontend.acp_recovery",
            error instanceof Error ? error.message : String(error),
          ).catch(() => {});
        });
      }
    };
    /** 在投递被共享水位接受后更新观测，重复、子任务和冷回放都不能污染当前轮。 */
    const observeLatency = (
      envelope: SessionUpdateDeliveryEnvelope | KeenCodeEventEnvelope,
      reduction: AcpDeliveryReduction,
      wasRecovering: boolean,
      receivedAtMs: number,
    ) => {
      const state = turnLatencyBySessionRef.current.get(envelope.sessionId);
      if (!state) return;
      const observed = observeTurnLatencyDelivery(
        state, envelope, reduction, wasRecovering, receivedAtMs,
      );
      if (observed !== state) turnLatencyBySessionRef.current.set(envelope.sessionId, observed);
    };
    const handleDelivery = (raw: unknown) => {
      const receivedAtMs = turnLatencyNow();
      const delivery = parseAcpTauriDelivery(raw);
      if (!delivery) {
        if (isRecord(raw) && raw.type === "client_request" &&
          isRecord(raw.request) &&
          raw.request.method === "elicitation/create" &&
          (typeof raw.request.id === "string" ||
            (typeof raw.request.id === "number" &&
              Number.isSafeInteger(raw.request.id)))) {
          void acpClientRespond(cancelledClientResponse(raw.request.id))
            .catch(() => {});
        }
        void diagnosticsRecord(
          "frontend.acp_delivery",
          "拒绝不符合当前严格契约的 Tauri 投递",
        ).catch(() => {});
        return;
      }
      if (delivery.type === "client_request") {
        showClientRequest(delivery.request);
        return;
      }
      if (delivery.type === "notification") {
        window.dispatchEvent(new CustomEvent<McpOAuthEvent>(
          KEENCODE_MCP_OAUTH_EVENT,
          { detail: delivery.notification.params },
        ));
        return;
      }
      if (delivery.type === "session_update") {
        const envelope = delivery.envelope;
        const view = ensureAcpSession(acpWorkspaceRef.current, envelope.sessionId);
        const wasRecovering = view.replay.throughDeliverySequence === null
          ? view.replay.restoring
          : envelope.deliverySequence <= view.replay.throughDeliverySequence;
        const hadVisibleMainText = view.live_segments.some(
          (segment) => (segment.kind === "thought" || segment.kind === "content") &&
            segment.text.trim().length > 0,
        );
        const reduction = reduceDeliveryEnvelope(view, envelope);
        if (reduction.status !== "applied") observeSessionDelivery(envelope.sessionId);
        if (reduction.status === "gap") {
          recoverGap(envelope.sessionId, reduction);
          flushProjection(envelope.sessionId);
          return;
        }
        if (reduction.status !== "applied") return;
        observeLatency(envelope, reduction, wasRecovering, receivedAtMs);
        applyUsageAndConfig(envelope, reduction.childAgentId);
        const update = envelope.update;
        if (shouldDriveMainSessionStreaming(update, Boolean(reduction.childAgentId))) {
          updateHostState(envelope.sessionId, "streaming");
        }
        const nowHasVisibleMainText = view.live_segments.some(
          (segment) => (segment.kind === "thought" || segment.kind === "content") &&
            segment.text.trim().length > 0,
        );
        if (!hadVisibleMainText && nowHasVisibleMainText) {
          flushProjection(envelope.sessionId);
        } else if (update.sessionUpdate === "agent_message_chunk" ||
          update.sessionUpdate === "agent_thought_chunk") {
          scheduleProjection(envelope.sessionId);
        } else {
          flushProjection(envelope.sessionId);
        }
        observeSessionDelivery(envelope.sessionId);
        return;
      }
      const envelope = delivery.envelope;
      const view = ensureAcpSession(acpWorkspaceRef.current, envelope.sessionId);
      // load响应和WebView事件回调可能跨队列到达；恢复标志已清除时，已确认
      // Journal水位以内仍是历史投递，不能补造实时耗时、未读或缓存刷新副作用。
      const wasRecovering = (view.replay.throughDeliverySequence === null
        ? view.replay.restoring
        : envelope.deliverySequence <= view.replay.throughDeliverySequence) ||
        (envelope.journalSequence !== undefined &&
          envelope.journalSequence <= view.replay.throughJournalSequence);
      const hadVisibleMainText = view.live_segments.some(
        (segment) => (segment.kind === "thought" || segment.kind === "content") &&
          segment.text.trim().length > 0,
      );
      const terminalRoot = isTerminalKeenCodeEvent(envelope.event) &&
        envelope.turnId === view.active_root_turn_id;
      const optimisticUser = terminalRoot
        ? appendOptimisticUser(
            view,
            messagesBySessionRef.current.get(envelope.sessionId) ?? [],
          )
        : undefined;
      const reduction = reduceDeliveryEnvelope(view, envelope);
      if (reduction.status !== "applied") observeSessionDelivery(envelope.sessionId);
      if (reduction.status === "gap") {
        recoverGap(envelope.sessionId, reduction);
        flushProjection(envelope.sessionId);
        return;
      }
      if (reduction.status !== "applied") return;
      observeLatency(envelope, reduction, wasRecovering, receivedAtMs);
      handleKeenCodeEvent(
        envelope,
        reduction,
        view,
        wasRecovering,
        hadVisibleMainText,
        optimisticUser,
        terminalRoot,
      );
      observeSessionDelivery(envelope.sessionId);
      flushProjection(envelope.sessionId);
    };

    void listenAcp("acp://delivery", (delivery) => {
      if (!disposed) handleDelivery(delivery);
    }).then((registered) => {
      if (disposed) registered();
      else unlisten = registered;
    }).catch((error) => {
      void diagnosticsRecord(
        "frontend.acp_delivery_listener",
        error instanceof Error ? error.message : String(error),
      ).catch(() => {});
    });

    return () => {
      disposed = true;
      projectionBatcher.cancel();
      unlisten?.();
    };
  }, [commitWorkspace, recoverSession, refreshTaskCacheUsage, observeSessionDelivery]);
}

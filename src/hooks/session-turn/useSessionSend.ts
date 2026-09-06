import { useCallback } from "react";
import type { Locale, MessageKey, Vars } from "@/i18n";
import type { SessionSnapshot } from "@/lib/session";
import { buildAgentPrompt } from "@/lib/attachments";
import {
  clearPriorTurnErrors,
  clearPriorTurnStreaming,
  localizeUiError,
} from "@/lib/session";
import { isDraftEmpty, parseStoredContent, serializeForAgent } from "@/lib/draftDoc";
import { beginLocalSessionTurn } from "@/lib/acp/store";
import {
  ensureAcpSession,
  replaceHistoryTurnMetrics,
} from "@/lib/acp/projection";
import { projectHostIntoLiveMap } from "@/lib/sessionLiveStore";
import {
  createTurnLatencyState,
  reduceTurnLatency,
  summarizeTurnLatency,
  turnLatencyNow,
} from "@/lib/turnLatency";
import { isViewingSendTarget } from "@/lib/viewFocus";
import type {
  EnsureConnected,
  ExecuteSend,
  SessionTurnApiPort,
  SessionTurnQueuePort,
  SessionTurnRuntimePort,
  SessionTurnState,
  SessionTurnUiPort,
} from "./types";

export interface UseSessionSendOptions {
  locale: Locale;
  tr: (key: MessageKey, vars?: Vars) => string;
  sessionId: string | null;
  modelLabel: string;
  hasConfiguredModel: boolean;
  api: SessionTurnApiPort;
  runtime: SessionTurnRuntimePort;
  ui: Pick<
    SessionTurnUiPort,
    | "setSession"
    | "setMessages"
    | "setLiveHost"
    | "setLiveMap"
    | "setRetryStatus"
    | "setTurnStartedAt"
    | "setLocalError"
    | "setPlanModeSessionKey"
    | "setUltraModeSessionKey"
  >;
  state: Pick<
    SessionTurnState,
    | "sendInFlightRef"
    | "turnLatencyBySessionRef"
    | "activeTurnIdBySessionRef"
    | "recoverableCompletedTurnIdBySessionRef"
    | "pendingVisibleTurnBySessionRef"
  >;
  ensureConnected: EnsureConnected;
  sendQueue: SessionTurnQueuePort;
}

export function useSessionSend({
  locale,
  tr,
  sessionId,
  modelLabel,
  hasConfiguredModel,
  api,
  runtime,
  ui,
  state,
  ensureConnected,
  sendQueue,
}: UseSessionSendOptions): ExecuteSend {
  const {
    acpWorkspaceRef,
    liveHostRef,
    messagesBySessionRef,
    viewingSessionIdRef,
    applyViewProjectionRef,
    commitWorkspace,
    patchSessionMessages,
    currentViewFocus,
    applyMessagePrefixTitle,
    applyAutomaticSessionTitle,
  } = runtime;
  const {
    setSession,
    setMessages,
    setLiveHost,
    setLiveMap,
    setRetryStatus,
    setTurnStartedAt,
    setLocalError,
    setPlanModeSessionKey,
    setUltraModeSessionKey,
  } = ui;
  const {
    sendInFlightRef,
    turnLatencyBySessionRef,
    activeTurnIdBySessionRef,
    recoverableCompletedTurnIdBySessionRef,
    pendingVisibleTurnBySessionRef,
  } = state;

  return useCallback<ExecuteSend>(
    async (options) => {
      if (sendInFlightRef.current) return false;
      sendInFlightRef.current = true;
      const {
        storedDisplay,
        att,
        createGoal = false,
        planMode = false,
        ultraMode = false,
        fromQueue,
        requestId: queuedRequestId,
      } = options;
      const segments = parseStoredContent(storedDisplay);
      if (isDraftEmpty(segments) && !att.length) {
        sendInFlightRef.current = false;
        return false;
      }
      if (!hasConfiguredModel) {
        sendInFlightRef.current = false;
        return false;
      }
      const sendTargetId =
        options.targetSessionId !== undefined
          ? options.targetSessionId
          : sessionId;
      const cacheKey = sendTargetId ?? "__draft__";
      const originView = currentViewFocus();
      const viewingTarget = () =>
        isViewingSendTarget(originView, currentViewFocus(), sendTargetId);
      const agentBody = serializeForAgent(segments);
      const agentText = buildAgentPrompt(agentBody, att);
      const optimisticDisplay = storedDisplay.trim();
      const turnStartedAtMs = turnLatencyNow();
      const ts = Math.floor(turnStartedAtMs);
      const userMessageId = `u-${ts}`;
      const pendingAssistantId = `a-pending-${ts}`;
      const requestId = queuedRequestId ?? globalThis.crypto.randomUUID();
      const dropIds = fromQueue
        ? new Set([userMessageId, pendingAssistantId])
        : new Set([pendingAssistantId]);
      const stripOptimistic = (messages: Parameters<typeof clearPriorTurnStreaming>[0]) =>
        messages.filter((message) => !dropIds.has(message.id));

      if (viewingTarget()) {
        setRetryStatus(null);
        // 新的有效发送接管当前可见会话；后台发送不能清除前台错误。
        setLocalError(null);
      }
      const nowIso = new Date().toISOString();
      const appendOptimistic = (messages: Parameters<typeof clearPriorTurnStreaming>[0]) => {
        const cleaned = clearPriorTurnErrors(
          clearPriorTurnStreaming(messages),
        );
        return [
          ...cleaned,
          {
            id: userMessageId,
            role: "user" as const,
            content: optimisticDisplay,
            model: modelLabel,
            attachments: att.length ? att : undefined,
            createdAt: nowIso,
          },
          {
            id: pendingAssistantId,
            role: "assistant" as const,
            content: "",
            streaming: true,
          },
        ];
      };
      if (sendTargetId) {
        patchSessionMessages(sendTargetId, appendOptimistic);
      } else if (viewingTarget()) {
        setMessages((messages) => {
          const next = appendOptimistic(messages);
          messagesBySessionRef.current.set(cacheKey, next);
          return next;
        });
      } else {
        const previous = messagesBySessionRef.current.get(cacheKey) ?? [];
        messagesBySessionRef.current.set(
          cacheKey,
          appendOptimistic(previous),
        );
      }
      setTurnStartedAt(ts);
      if (viewingTarget()) {
        setSession((previous) =>
          previous.state === "streaming"
            ? previous
            : { ...previous, state: "streaming", lastError: null },
        );
      }
      setLiveHost((previous) => {
        if (previous.sessionId) {
          if (sendTargetId && previous.sessionId !== sendTargetId) {
            return previous;
          }
          if (!sendTargetId && previous.sessionId) return previous;
        }
        const next = {
          ...previous,
          sessionId: sendTargetId ?? previous.sessionId,
          state: "streaming" as const,
          lastError: null,
        };
        liveHostRef.current = next;
        return next;
      });

      const failStrip = () => {
        if (sendTargetId) {
          patchSessionMessages(sendTargetId, stripOptimistic);
        } else {
          const draftMessages = messagesBySessionRef.current.get("__draft__");
          if (draftMessages) {
            messagesBySessionRef.current.set(
              "__draft__",
              stripOptimistic(draftMessages),
            );
          }
          if (viewingTarget()) setMessages((messages) => stripOptimistic(messages));
        }
        if (viewingTarget()) {
          setSession((previous) =>
            previous.state === "streaming"
              ? {
                  ...previous,
                  state: previous.sessionId ? "ready" : previous.state,
                }
              : previous,
          );
        }
        setLiveHost((previous) => {
          if (previous.sessionId) {
            if (sendTargetId && previous.sessionId !== sendTargetId) {
              return previous;
            }
            if (!sendTargetId && previous.sessionId) return previous;
          }
          if (previous.state !== "streaming") return previous;
          const next = {
            ...previous,
            state: (previous.sessionId ? "ready" : "idle") as SessionSnapshot["state"],
          };
          liveHostRef.current = next;
          return next;
        });
      };

      let latencySessionId: string | null = null;
      let transportFailureHandled = false;
      const handleTransportFailure = (cause: unknown) => {
        if (transportFailureHandled) return;
        transportFailureHandled = true;
        const currentActiveTurnId = latencySessionId
          ? activeTurnIdBySessionRef.current.get(latencySessionId)
          : undefined;
        // 新回合已经接管同一 Session 时，旧 Prompt 不能回写新回合状态。
        if (currentActiveTurnId && currentActiveTurnId !== requestId) return;
        if (latencySessionId) {
          const latency = turnLatencyBySessionRef.current.get(latencySessionId);
          if (latency?.turnId === requestId) {
            turnLatencyBySessionRef.current.delete(latencySessionId);
          }
          if (currentActiveTurnId === requestId) {
            activeTurnIdBySessionRef.current.delete(latencySessionId);
            const view = acpWorkspaceRef.current.sessions[latencySessionId];
            if (view) {
              view.status = "idle";
              view.turn_started_at = null;
              view.retry = null;
              commitWorkspace();
              if (viewingSessionIdRef.current === latencySessionId) {
                applyViewProjectionRef.current(latencySessionId);
              }
            }
          }
        }
        failStrip();
        if (viewingTarget()) setLocalError(localizeUiError(cause, locale));
      };
      try {
        let resolvedSessionId: string | null = null;
        const live = liveHostRef.current;
        if (
          sendTargetId &&
          live.sessionId === sendTargetId &&
          live.state === "ready" &&
          acpWorkspaceRef.current.sessions[sendTargetId]?.replay.loaded &&
          !acpWorkspaceRef.current.sessions[sendTargetId]?.replay.restoring &&
          !acpWorkspaceRef.current.sessions[sendTargetId]?.delivery.frozen &&
          !live.lastError
        ) {
          resolvedSessionId = sendTargetId;
        } else if (
          fromQueue &&
          sendTargetId &&
          viewingSessionIdRef.current !== sendTargetId
        ) {
          failStrip();
          return false;
        } else {
          resolvedSessionId = await ensureConnected({ sessionId: sendTargetId });
        }
        if (!resolvedSessionId) {
          failStrip();
          return false;
        }
        if (fromQueue && sendTargetId && resolvedSessionId !== sendTargetId) {
          failStrip();
          return false;
        }
        if (!sendTargetId) {
          const draftMessages = messagesBySessionRef.current.get("__draft__");
          if (draftMessages?.length) {
            messagesBySessionRef.current.set(resolvedSessionId, draftMessages);
            messagesBySessionRef.current.delete("__draft__");
          }
          if (planMode) setPlanModeSessionKey(resolvedSessionId);
          if (ultraMode) setUltraModeSessionKey(resolvedSessionId);
        }
        if (
          fromQueue &&
          sendTargetId &&
          liveHostRef.current.sessionId &&
          liveHostRef.current.sessionId !== sendTargetId
        ) {
          failStrip();
          return false;
        }
        const existingActiveTurnId = activeTurnIdBySessionRef.current.get(
          resolvedSessionId,
        );
        if (
          existingActiveTurnId &&
          existingActiveTurnId !== requestId
        ) {
          throw new Error("Session 正在运行，当前消息不能覆盖已有回合");
        }
        const acpView = ensureAcpSession(
          acpWorkspaceRef.current,
          resolvedSessionId,
        );
        beginLocalSessionTurn(acpView, ts);
        pendingVisibleTurnBySessionRef.current.delete(resolvedSessionId);
        recoverableCompletedTurnIdBySessionRef.current.delete(resolvedSessionId);
        activeTurnIdBySessionRef.current.set(resolvedSessionId, requestId);
        commitWorkspace();
        if (viewingSessionIdRef.current === resolvedSessionId) {
          applyViewProjectionRef.current(resolvedSessionId);
        }
        turnLatencyBySessionRef.current.set(
          resolvedSessionId,
          createTurnLatencyState(requestId, turnStartedAtMs),
        );
        latencySessionId = resolvedSessionId;
        if (createGoal) {
          const objective = agentBody.trim();
          if (!objective) throw new Error(tr("goal.objectiveRequired"));
          const result = await api.goalUpsert({
            sessionId: resolvedSessionId,
            goal: {
              title: objective,
              objective,
              description: objective,
            },
            expectedRevision: acpView.goal.revision,
            requestNonce: `${requestId}-goal`,
          });
          const view = ensureAcpSession(
            acpWorkspaceRef.current,
            resolvedSessionId,
          );
          view.goal = { revision: result.revision, goal: result.goal };
          commitWorkspace();
        }
        applyMessagePrefixTitle(resolvedSessionId, optimisticDisplay);
        void applyAutomaticSessionTitle(resolvedSessionId, optimisticDisplay);
        if (
          viewingSessionIdRef.current === resolvedSessionId ||
          viewingTarget()
        ) {
          setSession((previous) => ({
            ...previous,
            sessionId: resolvedSessionId,
            state: "streaming",
            lastError: null,
          }));
        }
        setLiveHost((previous) => {
          const next = {
            ...previous,
            sessionId: resolvedSessionId,
            state: "streaming" as const,
            lastError: null,
          };
          liveHostRef.current = next;
          return next;
        });
        const promptRun = api.send({
          text: agentText,
          sessionId: resolvedSessionId,
          requestId,
          planMode,
          ultraMode,
        });

        const started = await promptRun.started;
        // started 只代表权威 TurnStarted 已到达；不要持有整个 Prompt 的锁。
        sendInFlightRef.current = false;
        if (started.turnId !== requestId) {
          throw new Error("Host 返回了不匹配的 requestId");
        }
        const latency = turnLatencyBySessionRef.current.get(resolvedSessionId);
        if (latency?.turnId === requestId) {
          const acknowledgedLatency = reduceTurnLatency(latency, {
            type: "send_acknowledged",
            turnId: latency.turnId,
            // 与发送、首响应、正文到达及完成共用前端单调时钟；不混入宿主墙钟。
            atMs: turnLatencyNow(),
          });
          if (acknowledgedLatency.completedAtMs != null) {
            const view = acpWorkspaceRef.current.sessions[resolvedSessionId];
            if (
              view &&
              replaceHistoryTurnMetrics(
                view,
                summarizeTurnLatency(acknowledgedLatency),
              )
            ) {
              commitWorkspace();
              applyViewProjectionRef.current(viewingSessionIdRef.current);
            }
            if (
              pendingVisibleTurnBySessionRef.current.get(resolvedSessionId) ===
              acknowledgedLatency.turnId
            ) {
              turnLatencyBySessionRef.current.set(
                resolvedSessionId,
                acknowledgedLatency,
              );
            } else {
              turnLatencyBySessionRef.current.delete(resolvedSessionId);
            }
          } else {
            turnLatencyBySessionRef.current.set(
              resolvedSessionId,
              acknowledgedLatency,
            );
            setLiveMap((previous) =>
              projectHostIntoLiveMap(previous, {
                sessionId: resolvedSessionId!,
                state: "streaming",
                streamingMessageId: null,
              }),
            );
          }
        }
        if (!sendTargetId) sendQueue.bindDraft(resolvedSessionId);
        // 终态由共享 ACP 事件归约；仅传输失败复用本 Hook 的错误/恢复清理。
        void promptRun.completed.catch(handleTransportFailure);
        return true;
      } catch (cause) {
        handleTransportFailure(cause);
        return false;
      } finally {
        sendInFlightRef.current = false;
      }
    },
    [
      acpWorkspaceRef,
      api,
      applyAutomaticSessionTitle,
      applyMessagePrefixTitle,
      applyViewProjectionRef,
      commitWorkspace,
      currentViewFocus,
      ensureConnected,
      hasConfiguredModel,
      locale,
      liveHostRef,
      messagesBySessionRef,
      modelLabel,
      patchSessionMessages,
      recoverableCompletedTurnIdBySessionRef,
      sendInFlightRef,
      sendQueue,
      sessionId,
      setLiveHost,
      setLiveMap,
      setLocalError,
      setMessages,
      setPlanModeSessionKey,
      setRetryStatus,
      setSession,
      setTurnStartedAt,
      setUltraModeSessionKey,
      tr,
      turnLatencyBySessionRef,
      activeTurnIdBySessionRef,
      pendingVisibleTurnBySessionRef,
      viewingSessionIdRef,
    ],
  );
}

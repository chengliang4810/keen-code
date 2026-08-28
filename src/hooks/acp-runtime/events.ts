import { useEffect } from "react";
import * as api from "@/lib/api";
import type {
  AskUserPayload,
  ChatMessage,
  SessionSnapshot,
} from "@/lib/session";
import type { SessionContextUsage } from "@/features/app/models";
import {
  diagnosticsRecord,
  listenAcp,
  sessionGetState,
  sessionResolveAskUser,
} from "@/lib/acp/api";
import {
  parseElicitationPayload,
  readElicitationRpcId,
} from "@/lib/elicitation";
import {
  commitLiveTurnToHistory,
  ensureAcpSession,
  reduceReplayedSessionUpdate,
} from "@/lib/acp/projection";
import {
  reduceAgentEvent,
  reduceRecovery,
  reduceSessionUpdate,
  resolveSessionUpdateSourceAgentId,
  type AcpWorkspaceState,
} from "@/lib/acp/store";
import {
  isForegroundRequestDone,
  isReplayedUpdate,
  isRequestScopedAgentEvent,
  isRequestScopedSessionUpdate,
  parseAgentEvent,
  shouldAcceptAgentDone,
  shouldDriveMainSessionStreaming,
  shouldApplyAgentEvent,
  shouldApplySessionUpdate,
} from "@/lib/acp/events";
import {
  projectHostIntoLiveMap,
  type SessionLiveMap,
} from "@/lib/sessionLiveStore";
import {
  reduceTurnLatency,
  summarizeTurnLatency,
  turnUsageActionFromAcp,
  type TurnLatencyState,
} from "@/lib/turnLatency";
import {
  createActiveTurnBootstrapBuffer,
  resolveActiveTurnFromHostSnapshot,
} from "@/lib/activeTurn";
import {
  isNormalSessionCompletion,
  saveCompletedUnreadSessionIds,
} from "@/lib/sessionCompletion";
import { createAnimationFrameBatcher } from "@/lib/frameBatcher";
import type { Ref, SetState, ViewProjection } from "./types";

export interface AcpRuntimeEventsOptions {
  commitWorkspace: () => void;
  acpWorkspaceRef: Ref<AcpWorkspaceState>;
  turnLatencyBySessionRef: Ref<Map<string, TurnLatencyState>>;
  activeTurnIdBySessionRef: Ref<Map<string, string>>;
  recoverableCompletedTurnIdBySessionRef: Ref<Map<string, string>>;
  completedTurnIdBySessionRef: Ref<Map<string, string>>;
  pendingVisibleTurnBySessionRef: Ref<Map<string, string>>;
  liveHostRef: Ref<SessionSnapshot>;
  messagesBySessionRef: Ref<Map<string, ChatMessage[]>>;
  modelBySessionRef: Ref<Map<string, string>>;
  contextUsageBySessionRef: Ref<Map<string, SessionContextUsage>>;
  viewingSessionIdRef: Ref<string | null>;
  configuredModelsRef: Ref<Array<{ id: string }>>;
  clearPendingAskUserRef: Ref<
    (sessionId?: string | null, rpcId?: number) => void
  >;
  pendingAskUserBySessionRef: Ref<Map<string, AskUserPayload>>;
  setPendingAskUserSessionIds: SetState<Set<string>>;
  setAskUser: SetState<AskUserPayload | null>;
  setContextUsage: SetState<SessionContextUsage | null>;
  setLiveHost: SetState<SessionSnapshot>;
  setLiveMap: SetState<SessionLiveMap>;
  setTurnStartedAt: SetState<number | null>;
  setModelId: SetState<string>;
  setCompletedUnreadIds: SetState<Set<string>>;
  applyViewProjectionRef: Ref<ViewProjection>;
  refreshTaskCacheUsage: (sessionId: string | null) => Promise<void>;
}

/**
 * 订阅 ACP 事件、归约工作区并在绘制边界投影当前 Session。
 * 监听器注册和 Host active-turn 恢复必须在同一个生命周期内完成，避免
 * 快速卸载/重挂时把旧事件重放到新订阅者。
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
}: AcpRuntimeEventsOptions): void {
  useEffect(() => {
    if (!api.isTauri()) return;
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const pendingProjectionSessions = new Set<string>();
    const publishScheduledEvents = () => {
      if (disposed) return;
      const viewingSessionId = viewingSessionIdRef.current;
      const shouldProjectViewing =
        viewingSessionId != null &&
        pendingProjectionSessions.has(viewingSessionId);
      pendingProjectionSessions.clear();
      commitWorkspace();
      if (shouldProjectViewing) {
        applyViewProjectionRef.current(viewingSessionId);
      }
    };
    const projectionBatcher = createAnimationFrameBatcher(
      publishScheduledEvents,
      (callback) => requestAnimationFrame(callback),
      (id) => cancelAnimationFrame(id),
    );
    const scheduleProjection = (sessionId: string) => {
      pendingProjectionSessions.add(sessionId);
      projectionBatcher.schedule();
    };
    const flushProjection = (sessionId: string) => {
      pendingProjectionSessions.add(sessionId);
      if (viewingSessionIdRef.current === sessionId) {
        projectionBatcher.flush();
        return;
      }
      // 后台 Session 的边界并入下一绘制帧，不能借机提前发布当前会话
      // 尚在等待绘制帧的 text/thought；liveMap 已单独同步关键忙闲状态。
      projectionBatcher.schedule();
    };
    const activeTurnsBeforeBootstrap = new Map(
      activeTurnIdBySessionRef.current,
    );
    const correlatedTurnId = (sessionId: string) =>
      activeTurnIdBySessionRef.current.get(sessionId) ??
      recoverableCompletedTurnIdBySessionRef.current.get(sessionId);
    const activeTurnBootstrap =
      createActiveTurnBootstrapBuffer(correlatedTurnId);
    const registrationPromises: Array<Promise<() => void>> = [];
    void (async () => {
      registrationPromises.push(
        listenAcp("acp://session-update", (notification) => {
          if (disposed) return;
          const params = notification.params;
          if (!params?.sessionId) return;
          const view = ensureAcpSession(
            acpWorkspaceRef.current,
            params.sessionId,
          );
          const sourceAgentId = resolveSessionUpdateSourceAgentId(
            view,
            params._peri?.sourceAgentId,
          );
          const apply = () => {
            const activeRequestId = correlatedTurnId(params.sessionId);
            if (
              !shouldApplySessionUpdate(
                params,
                activeRequestId,
                sourceAgentId,
              )
            ) {
              return;
            }
            const tag = params.update.sessionUpdate;
            const hadVisibleMainText = view.live_segments.some(
              (segment) =>
                (segment.kind === "thought" || segment.kind === "content") &&
                segment.text.trim().length > 0,
            );
            if (
              tag === "usage_update" &&
              !sourceAgentId &&
              Number.isFinite(params.update.used) &&
              params.update.used >= 0 &&
              Number.isFinite(params.update.size) &&
              params.update.size > 0
            ) {
              const usage: SessionContextUsage = {
                used: params.update.used,
                size: params.update.size,
                estimated: params.update._meta?.estimated === true,
              };
              contextUsageBySessionRef.current.set(params.sessionId, usage);
              if (viewingSessionIdRef.current === params.sessionId) {
                setContextUsage(usage);
              }
            }
            if (!sourceAgentId) {
              let latency = turnLatencyBySessionRef.current.get(
                params.sessionId,
              );
              if (latency && tag === "usage_update") {
                const usageAction = turnUsageActionFromAcp(
                  latency.turnId,
                  params.update,
                );
                if (usageAction) {
                  latency = reduceTurnLatency(latency, usageAction);
                  turnLatencyBySessionRef.current.set(
                    params.sessionId,
                    latency,
                  );
                }
              }
            }
            const replayed = isReplayedUpdate(params.update);
            if (replayed) {
              reduceReplayedSessionUpdate(
                view,
                params.update,
                sourceAgentId,
              );
            } else {
              // peri 无独立 turn_started 事件：实时内容块到达即视为 turn 进行中。
              // 新一轮开始：先兜底提交上一轮残留的实时文本（保持 history 顺序），
              // 再归约本条更新。
              if (
                tag === "user_message_chunk" &&
                !sourceAgentId &&
                view.status !== "streaming"
              ) {
                commitLiveTurnToHistory(view, {
                  thinkingDurationMs:
                    view.turn_started_at != null
                      ? Date.now() - view.turn_started_at
                      : undefined,
                });
                view.turn_started_at = null;
              }
              reduceSessionUpdate(
                view,
                params.update,
                sourceAgentId,
              );
              if (shouldDriveMainSessionStreaming(params.update, sourceAgentId)) {
                view.status = "streaming";
              }
            }
            if (tag === "config_option_update") {
              // 会话级模型恢复：只在模型仍存在于已配置目录时更新当前 composer。
              const modelOption = (params.update.configOptions ?? []).find(
                (option) => (option as { id?: unknown }).id === "model",
              );
              const modelValue = (
                modelOption as { currentValue?: unknown } | undefined
              )?.currentValue;
              if (typeof modelValue === "string" && modelValue.length > 0) {
                modelBySessionRef.current.set(params.sessionId, modelValue);
              }
              if (
                typeof modelValue === "string" &&
                modelValue.length > 0 &&
                viewingSessionIdRef.current === params.sessionId &&
                configuredModelsRef.current.some((m) => m.id === modelValue)
              ) {
                setModelId(modelValue);
              }
            }
            if (
              !replayed &&
              (tag === "agent_message_chunk" ||
                tag === "agent_thought_chunk")
            ) {
              const firstMainTextDelta =
                !sourceAgentId &&
                !hadVisibleMainText &&
                params.update.content.type === "text" &&
                params.update.content.text.trim().length > 0;
              if (firstMainTextDelta) flushProjection(params.sessionId);
              else scheduleProjection(params.sessionId);
            } else {
              flushProjection(params.sessionId);
            }
          };
          if (
            isRequestScopedSessionUpdate(params, sourceAgentId) &&
            activeTurnBootstrap.deferUnknown(
              params.sessionId,
              params.requestId,
              apply,
            )
          ) {
            return;
          }
          apply();
        }),
      );
      registrationPromises.push(
        listenAcp("acp://unstable-event", (notification) => {
          if (disposed) return;
          const params = notification.params;
          if (
            !params?.sessionId ||
            params.event !== "first-provider-event"
          ) {
            return;
          }
          const requestId = params.requestId;
          if (!requestId) return;
          if (
            typeof params.data?.source_agent_id === "string" &&
            params.data.source_agent_id.length > 0
          ) {
            return;
          }
          const sourceAtMs = params.data?.at_ms;
          if (
            typeof sourceAtMs !== "number" ||
            !Number.isFinite(sourceAtMs)
          ) {
            return;
          }
          const latency = turnLatencyBySessionRef.current.get(
            params.sessionId,
          );
          if (!latency || latency.turnId !== requestId) return;
          turnLatencyBySessionRef.current.set(
            params.sessionId,
            reduceTurnLatency(latency, {
              type: "first_sse",
              turnId: latency.turnId,
              atMs: sourceAtMs,
            }),
          );
        }),
      );
      registrationPromises.push(
        listenAcp("acp://agent-event", (notification) => {
          if (disposed) return;
          const params = notification.params;
          if (!params) return;
          const event = parseAgentEvent(params.event_json);
          if (!event) return;
          // OAuth 是 host 级事件且 sessionId 为空；交互入口由独立 MCP OAuth 功能处理。
          if (!params.sessionId) return;
          const apply = () => {
            const activeRequestId = correlatedTurnId(params.sessionId);
            if (!shouldApplyAgentEvent(params, event, activeRequestId)) {
              return;
            }
            const view = ensureAcpSession(
              acpWorkspaceRef.current,
              params.sessionId,
            );
            reduceAgentEvent(view, event);
            if (
              event.type === "turn_suspended" &&
              viewingSessionIdRef.current === params.sessionId
            ) {
              setTurnStartedAt(null);
            }
            flushProjection(params.sessionId);
          };
          if (
            isRequestScopedAgentEvent(event) &&
            activeTurnBootstrap.deferUnknown(
              params.sessionId,
              params.requestId,
              apply,
            )
          ) {
            return;
          }
          apply();
        }),
      );
      registrationPromises.push(
        listenAcp("acp://recovery-status", (notification) => {
          if (disposed) return;
          const params = notification.params;
          if (!params?.session_id) return;
          const view = ensureAcpSession(
            acpWorkspaceRef.current,
            params.session_id,
          );
          reduceRecovery(view, params);
          flushProjection(params.session_id);
        }),
      );
      registrationPromises.push(
        listenAcp("acp://elicitation", (notification) => {
          if (disposed) return;
          const payload = parseElicitationPayload(notification);
          if (!payload) {
            const rpcId = readElicitationRpcId(notification);
            if (rpcId != null) {
              void sessionResolveAskUser({
                rpcId,
                decision: "cancelled",
              }).catch(() => {});
            }
            return;
          }
          const pending = pendingAskUserBySessionRef.current.get(
            payload.sessionId,
          );
          if (pending && pending.rpcId !== payload.rpcId) {
            // 当前弹窗一次只能可靠承载一个表单；拒绝并发请求，避免静默覆盖。
            void sessionResolveAskUser({
              rpcId: payload.rpcId,
              decision: "cancelled",
            }).catch(() => {});
            return;
          }
          pendingAskUserBySessionRef.current.set(payload.sessionId, payload);
          setPendingAskUserSessionIds((previous) => {
            if (previous.has(payload.sessionId)) return previous;
            const next = new Set(previous);
            next.add(payload.sessionId);
            return next;
          });
          if (viewingSessionIdRef.current === payload.sessionId) {
            setAskUser(payload);
          }
        }),
      );
      registrationPromises.push(
        listenAcp("acp://agent-done", (notification) => {
          if (disposed) return;
          const params = notification.params;
          if (!params?.sessionId || !isForegroundRequestDone(params)) {
            return;
          }
          const apply = () => {
            if (
              completedTurnIdBySessionRef.current.get(params.sessionId) ===
              params.requestId
            ) {
              return;
            }
            const activeTurnId = activeTurnIdBySessionRef.current.get(
              params.sessionId,
            );
            const expectedTurnId = correlatedTurnId(params.sessionId);
            const activeLatency = turnLatencyBySessionRef.current.get(
              params.sessionId,
            );
            if (
              !shouldAcceptAgentDone(expectedTurnId, params.requestId) ||
              (activeLatency && activeLatency.turnId !== params.requestId)
            ) {
              return;
            }
            completedTurnIdBySessionRef.current.set(
              params.sessionId,
              params.requestId,
            );
            if (activeTurnId === params.requestId) {
              activeTurnIdBySessionRef.current.delete(params.sessionId);
            }
            if (
              recoverableCompletedTurnIdBySessionRef.current.get(
                params.sessionId,
              ) === params.requestId
            ) {
              recoverableCompletedTurnIdBySessionRef.current.delete(
                params.sessionId,
              );
            }
            const view = acpWorkspaceRef.current.sessions[params.sessionId];
            const awaitsVisibleToken =
              view?.live_segments.some(
                (segment) =>
                  (segment.kind === "thought" ||
                    segment.kind === "content") &&
                  segment.text.trim().length > 0,
              ) === true;
            const normalCompletion = isNormalSessionCompletion(
              params.stopReason,
              Boolean(view?.last_error),
            );
            const completedLatency = activeLatency
              ? reduceTurnLatency(activeLatency, {
                  type: "completed",
                  turnId: activeLatency.turnId,
                  atMs: params._keencode.completedAtMs,
                })
              : null;
            const turnMetrics = completedLatency
              ? summarizeTurnLatency(completedLatency)
              : undefined;
            const waitForVisibleCommit = Boolean(
              completedLatency &&
                completedLatency.firstVisibleTokenAtMs == null &&
                awaitsVisibleToken &&
                viewingSessionIdRef.current === params.sessionId,
            );
            if (waitForVisibleCommit && completedLatency) {
              pendingVisibleTurnBySessionRef.current.set(
                params.sessionId,
                completedLatency.turnId,
              );
            } else {
              pendingVisibleTurnBySessionRef.current.delete(params.sessionId);
            }
            if (view) {
              const optimisticUser = (
                messagesBySessionRef.current.get(params.sessionId) ?? []
              )
                .slice()
                .reverse()
                .find(
                  (message) =>
                    message.role === "user" && message.id.startsWith("u-"),
                );
              // 完成的实时 Turn 提交进 history，保证转写与自动标题在 turn 边界不丢失。
              commitLiveTurnToHistory(view, {
                userContent: optimisticUser?.content,
                thinkingDurationMs:
                  view.turn_started_at != null
                    ? Date.now() - view.turn_started_at
                    : undefined,
                turnMetrics,
                model: optimisticUser?.model,
              });
              view.turn_started_at = null;
              view.status = "idle";
              view.retry = null;
              // 正常完成后计划已失去操作价值；取消、停止与异常保留现场。
              if (normalCompletion) {
                view.todos = {
                  revision: view.todos.revision + 1,
                  items: [],
                };
              }
            }
            if (
              normalCompletion &&
              viewingSessionIdRef.current !== params.sessionId
            ) {
              setCompletedUnreadIds((previous) => {
                if (previous.has(params.sessionId)) return previous;
                const next = new Set(previous);
                next.add(params.sessionId);
                saveCompletedUnreadSessionIds(next, localStorage);
                return next;
              });
            }
            // 完成通知必须直接清理目标 Session 的后台运行投影，不能依赖当前查看页。
            setLiveMap((previous) =>
              projectHostIntoLiveMap(previous, {
                sessionId: params.sessionId,
                state: "ready",
                streamingMessageId: null,
              }),
            );
            setLiveHost((previous) => {
              if (previous.sessionId !== params.sessionId) return previous;
              const next = {
                ...previous,
                state: "ready" as const,
                streamingMessageId: null,
              };
              liveHostRef.current = next;
              return next;
            });
            if (viewingSessionIdRef.current === params.sessionId) {
              setTurnStartedAt(null);
            }
            if (
              completedLatency &&
              (completedLatency.sendAcknowledgedAtMs == null ||
                waitForVisibleCommit)
            ) {
              // invoke 响应和 Tauri 事件没有跨通道 happens-before。保留已完成
              // 状态，等迟到的 acceptedAtMs 补写历史；它绝不能把回合重开。
              turnLatencyBySessionRef.current.set(
                params.sessionId,
                completedLatency,
              );
            } else {
              turnLatencyBySessionRef.current.delete(params.sessionId);
            }
            clearPendingAskUserRef.current(params.sessionId);
            setAskUser((current) =>
              current?.sessionId === params.sessionId ? null : current,
            );
            if (viewingSessionIdRef.current === params.sessionId) {
              void refreshTaskCacheUsage(params.sessionId);
            }
            flushProjection(params.sessionId);
          };
          const deferred = activeTurnBootstrap.deferUnknown(
            params.sessionId,
            params.requestId,
            apply,
          );
          if (deferred) {
            return;
          }
          apply();
        }),
      );
      const registered = await Promise.all(registrationPromises);
      if (disposed) {
        for (const unlisten of registered) unlisten();
        return;
      }
      unlisteners.push(...registered);
      try {
        const runtimeState = await sessionGetState();
        if (!disposed) {
          const hostActiveTurns = new Map(
            runtimeState.activeTurns.map(({ sessionId, turnId }) => [
              sessionId,
              turnId,
            ]),
          );
          const hostCompletedTurns = new Map(
            runtimeState.completedTurns.map(({ sessionId, turnId }) => [
              sessionId,
              turnId,
            ]),
          );
          const recoverySessionIds = new Set([
            ...recoverableCompletedTurnIdBySessionRef.current.keys(),
            ...hostCompletedTurns.keys(),
            ...hostActiveTurns.keys(),
          ]);
          for (const sessionId of recoverySessionIds) {
            const completedTurnId = hostCompletedTurns.get(sessionId) ?? null;
            const currentTurnId =
              activeTurnIdBySessionRef.current.get(sessionId) ?? null;
            const locallyStartedTurnId =
              currentTurnId &&
              currentTurnId !==
                (activeTurnsBeforeBootstrap.get(sessionId) ?? null)
                ? currentTurnId
                : null;
            if (
              !activeTurnBootstrap.overflowed &&
              !hostActiveTurns.has(sessionId) &&
              !locallyStartedTurnId &&
              completedTurnId &&
              completedTurnIdBySessionRef.current.get(sessionId) !==
                completedTurnId
            ) {
              // Host 已完成但 Tauri done 可能仍在另一通道排队。只允许该精确
              // turn 的尾随事件通过；done handler 随即删除此恢复关联。
              recoverableCompletedTurnIdBySessionRef.current.set(
                sessionId,
                completedTurnId,
              );
            } else {
              recoverableCompletedTurnIdBySessionRef.current.delete(sessionId);
            }
          }
          const sessionIds = new Set([
            ...activeTurnIdBySessionRef.current.keys(),
            ...hostActiveTurns.keys(),
          ]);
          for (const sessionId of sessionIds) {
            const currentTurnId =
              activeTurnIdBySessionRef.current.get(sessionId) ?? null;
            const turnBeforeBootstrap =
              activeTurnsBeforeBootstrap.get(sessionId) ?? null;
            const locallyStartedTurnId =
              currentTurnId && currentTurnId !== turnBeforeBootstrap
                ? currentTurnId
                : null;
            const resolved = resolveActiveTurnFromHostSnapshot({
              snapshotTurnId: hostActiveTurns.get(sessionId) ?? null,
              localTurnId: locallyStartedTurnId,
              completedTurnId:
                completedTurnIdBySessionRef.current.get(sessionId) ?? null,
            });
            if (resolved) {
              activeTurnIdBySessionRef.current.set(sessionId, resolved);
            } else {
              activeTurnIdBySessionRef.current.delete(sessionId);
            }
          }
          activeTurnBootstrap.replayMatching();
          if (activeTurnBootstrap.overflowed) {
            void diagnosticsRecord(
              "frontend.active_turn_bootstrap",
              "恢复窗口事件超过 4096 条，已丢弃溢出事件",
            ).catch(() => {});
          }
          setLiveMap((previous) => {
            let next = previous;
            for (const sessionId of hostCompletedTurns.keys()) {
              if (activeTurnIdBySessionRef.current.has(sessionId)) continue;
              next = projectHostIntoLiveMap(next, {
                sessionId,
                state: "ready",
                streamingMessageId: null,
              });
            }
            for (const sessionId of activeTurnIdBySessionRef.current.keys()) {
              next = projectHostIntoLiveMap(next, {
                sessionId,
                state: "streaming",
                streamingMessageId: null,
              });
            }
            return next;
          });
        }
      } catch {
        activeTurnBootstrap.discard();
        // 后续 sessionConnect 会返回该 Session 的权威 activeTurnId。
      }
    })();
    return () => {
      disposed = true;
      projectionBatcher.cancel();
      for (const unlisten of unlisteners) unlisten();
    };
  }, [commitWorkspace, refreshTaskCacheUsage]);
}

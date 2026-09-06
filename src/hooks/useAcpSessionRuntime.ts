import { useCallback, useEffect } from "react";
import type {
  UseAcpSessionRuntimeOptions,
  UseAcpSessionRuntimeResult,
} from "./acp-runtime/types";
import { useAcpRuntimeEvents } from "./acp-runtime/events";
import { useAcpRuntimeHistory } from "./acp-runtime/history";
import { useAcpRuntimeMessageCache } from "./acp-runtime/messageCache";
import { useAcpRuntimeProjection } from "./acp-runtime/projection";
import { useAcpRuntimeTaskCache } from "./acp-runtime/taskCache";
import { useAcpRuntimeTurnMetrics } from "./acp-runtime/turnMetrics";
import { invalidateSessionContextUsage } from "@/lib/contextUsage";

export type {
  UseAcpSessionRuntimeOptions,
  UseAcpSessionRuntimeResult,
} from "./acp-runtime/types";

export function useAcpSessionRuntime(
  options: UseAcpSessionRuntimeOptions,
): UseAcpSessionRuntimeResult {
  const {
    locale,
    session,
    messages,
    liveHost,
    acpWorkspace,
    commitWorkspace,
    acpWorkspaceRef,
    turnLatencyBySessionRef,
    activeTurnIdBySessionRef,
    recoverableCompletedTurnIdBySessionRef,
    completedTurnIdBySessionRef,
    pendingVisibleTurnBySessionRef,
    liveHostRef,
    messagesRef,
    messagesBySessionRef,
    modelBySessionRef,
    contextUsageBySessionRef,
    taskCacheUsageRequestSeqRef,
    viewingSessionIdRef,
    openingSessionIdRef,
    currentViewFocus,
    sessionTitleOverridesRef,
    sessionsRef,
    sendInFlightRef,
    configuredModelsRef,
    clearPendingAskUserRef,
    pendingAskUserBySessionRef,
    setPendingAskUserSessionIds,
    setAskUser,
    setSession,
    setMessages,
    setLiveHost,
    setLiveMap,
    setContextUsage,
    setTaskCacheUsage,
    setRetryStatus,
    setTurnStartedAt,
    setEffort,
    setModelId,
    setPlanModeSessionKey,
    promptHistoryIndexRef,
    setPromptHistoryIndex,
    setPromptHistoryOpen,
    setPromptHistoryFilter,
    setPromptHistoryActive,
    setPromptHistoryFocusFilter,
    setCompletedUnreadIds,
  } = options;

  /** 清除指定 Session 的缓存用量，并同步清空当前可见值。 */
  const invalidateContextUsage = useCallback((sessionId: string) => {
    invalidateSessionContextUsage(contextUsageBySessionRef.current, sessionId);
    if (viewingSessionIdRef.current === sessionId) {
      // 使尚未返回的旧任务缓存查询失效，避免它在清理后把旧用量写回。
      taskCacheUsageRequestSeqRef.current += 1;
      setContextUsage(null);
    }
  }, []);

  const { refreshTaskCacheUsage } = useAcpRuntimeTaskCache({
    session,
    viewingSessionIdRef,
    contextUsageBySessionRef,
    taskCacheUsageRequestSeqRef,
    setContextUsage,
    setTaskCacheUsage,
  });

  const { applyViewProjection, applyViewProjectionRef } =
    useAcpRuntimeProjection({
      locale,
      acpWorkspaceRef,
      activeTurnIdBySessionRef,
      contextUsageBySessionRef,
      sessionTitleOverridesRef,
      sessionsRef,
      sendInFlightRef,
      liveHostRef,
      messagesBySessionRef,
      setContextUsage,
      setSession,
      setMessages,
      setLiveHost,
      setLiveMap,
      setRetryStatus,
      setEffort,
    });

  const { replayHistory, recoverSession, observeSessionDelivery, connectSession } = useAcpRuntimeHistory({
    acpWorkspaceRef,
    turnLatencyBySessionRef,
    pendingVisibleTurnBySessionRef,
    applyViewProjectionRef,
    commitWorkspace,
    currentViewFocus,
    invalidateContextUsage,
    setPlanModeSessionKey,
  });

  useAcpRuntimeEvents({
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
  });

  const { handleFirstVisibleToken } = useAcpRuntimeTurnMetrics({
    sessionId: session.sessionId,
    acpWorkspaceRef,
    turnLatencyBySessionRef,
    pendingVisibleTurnBySessionRef,
    viewingSessionIdRef,
    commitWorkspace,
    applyViewProjection: applyViewProjectionRef,
  });

  const { patchSessionMessages } = useAcpRuntimeMessageCache({
    session,
    messages,
    messagesRef,
    messagesBySessionRef,
    viewingSessionIdRef,
    setMessages,
  });

  // Keep refs aligned for event handlers, but not while openSession is loading.
  useEffect(() => {
    if (openingSessionIdRef.current) return;
    viewingSessionIdRef.current = session.sessionId;
  }, [session.sessionId]);

  // Prompt history is scoped to the viewed Session.
  useEffect(() => {
    promptHistoryIndexRef.current = null;
    setPromptHistoryIndex(null);
    setPromptHistoryOpen(false);
    setPromptHistoryFilter("");
    setPromptHistoryActive(0);
    setPromptHistoryFocusFilter(false);
  }, [session.sessionId]);

  useEffect(() => {
    liveHostRef.current = liveHost;
  }, [liveHost]);

  return {
    acpWorkspaceRef,
    acpWorkspace,
    turnLatencyBySessionRef,
    activeTurnIdBySessionRef,
    recoverableCompletedTurnIdBySessionRef,
    completedTurnIdBySessionRef,
    pendingVisibleTurnBySessionRef,
    liveHostRef,
    messagesRef,
    messagesBySessionRef,
    contextUsageBySessionRef,
    taskCacheUsageRequestSeqRef,
    invalidateContextUsage,
    refreshTaskCacheUsage,
    applyViewProjection,
    applyViewProjectionRef,
    handleFirstVisibleToken,
    replayHistory,
    connectSession,
    patchSessionMessages,
  };
}

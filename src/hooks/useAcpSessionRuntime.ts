import { useEffect } from "react";
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
    promptHistoryIndexRef,
    setPromptHistoryIndex,
    setPromptHistoryOpen,
    setPromptHistoryFilter,
    setPromptHistoryActive,
    setPromptHistoryFocusFilter,
    setCompletedUnreadIds,
  } = options;

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

  const { replayHistory } = useAcpRuntimeHistory({
    acpWorkspaceRef,
    applyViewProjectionRef,
    commitWorkspace,
    currentViewFocus,
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
    refreshTaskCacheUsage,
    applyViewProjection,
    applyViewProjectionRef,
    handleFirstVisibleToken,
    replayHistory,
    patchSessionMessages,
  };
}

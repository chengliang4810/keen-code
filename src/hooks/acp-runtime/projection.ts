import { useCallback, useRef } from "react";
import type { Locale } from "@/i18n";
import type { ChatMessage, SessionSnapshot } from "@/lib/session";
import {
  projectHostIntoLiveMap,
  type SessionLiveMap,
} from "@/lib/sessionLiveStore";
import {
  projectAcpConversation,
  projectAcpSnapshot,
} from "@/lib/sessionProjection";
import type { AcpWorkspaceState } from "@/lib/acp/store";
import type { SessionContextUsage, SessionRow } from "@/features/app/models";
import type { ViewProjection, Ref, SetState } from "./types";

export interface AcpRuntimeProjectionOptions {
  locale: Locale;
  acpWorkspaceRef: Ref<AcpWorkspaceState>;
  activeTurnIdBySessionRef: Ref<Map<string, string>>;
  contextUsageBySessionRef: Ref<Map<string, SessionContextUsage>>;
  sessionTitleOverridesRef: Ref<Map<string, string>>;
  sessionsRef: Ref<SessionRow[]>;
  sendInFlightRef: Ref<boolean>;
  liveHostRef: Ref<SessionSnapshot>;
  messagesBySessionRef: Ref<Map<string, ChatMessage[]>>;
  setContextUsage: SetState<SessionContextUsage | null>;
  setSession: SetState<SessionSnapshot>;
  setMessages: SetState<ChatMessage[]>;
  setLiveHost: SetState<SessionSnapshot>;
  setLiveMap: SetState<SessionLiveMap>;
  setRetryStatus: SetState<{
    attempt: number;
    maxAttempts: number;
    delayMs: number;
    reason: string;
  } | null>;
  setEffort: SetState<string>;
}

export function useAcpRuntimeProjection({
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
}: AcpRuntimeProjectionOptions): {
  applyViewProjection: ViewProjection;
  applyViewProjectionRef: Ref<ViewProjection>;
} {
  /** 将 acpWorkspace 中指定 Session 的最新投影应用到工作台组件。 */
  const applyViewProjection = useCallback((sessionId: string | null) => {
    if (!sessionId) return;
    const view = acpWorkspaceRef.current.sessions[sessionId];
    if (!view) return;
    const projectedSnapshot = projectAcpSnapshot(view);
    const preferredTitle =
      sessionTitleOverridesRef.current.get(sessionId) ??
      sessionsRef.current.find((row) => row.id === sessionId)?.title;
    const snapshot = preferredTitle
      ? { ...projectedSnapshot, title: preferredTitle }
      : projectedSnapshot;
    const reportedUsage = contextUsageBySessionRef.current.get(sessionId);
    setContextUsage(reportedUsage ?? null);
    setSession(snapshot);
    setRetryStatus(
      view.retry
        ? {
            attempt: view.retry.attempt,
            maxAttempts: view.retry.maxAttempts,
            delayMs: view.retry.delayMs,
            reason: view.retry.reason,
          }
        : null,
    );
    if (view.reasoning_effort) setEffort(view.reasoning_effort);
    setLiveHost(snapshot);
    liveHostRef.current = snapshot;
    setLiveMap((previous) =>
      projectHostIntoLiveMap(previous, {
        sessionId,
        state: snapshot.state,
        streamingMessageId: snapshot.streamingMessageId,
      }),
    );
    setMessages((previous) => {
      const hasLocalPendingAssistant = previous.some(
        (message) =>
          message.role === "assistant" &&
          message.id.startsWith("a-pending-") &&
          message.streaming === true,
      );
      const keepPendingAssistant =
        view.status === "streaming" ||
        activeTurnIdBySessionRef.current.has(sessionId) ||
        (sendInFlightRef.current && hasLocalPendingAssistant);
      const next = projectAcpConversation(
        previous,
        view,
        locale,
        keepPendingAssistant,
      );
      messagesBySessionRef.current.set(sessionId, next);
      return next;
    });
  }, [locale]);

  /** 事件监听与异步流程用最新 applyViewProjection。 */
  const applyViewProjectionRef = useRef(applyViewProjection);
  applyViewProjectionRef.current = applyViewProjection;

  return { applyViewProjection, applyViewProjectionRef };
}

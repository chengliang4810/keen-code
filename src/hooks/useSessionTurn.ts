import { useMemo, useRef } from "react";
import type { StopLatchState } from "@/lib/stopLatch";
import {
  canSendWithStopLatch,
  canStopWithStopLatch,
} from "@/lib/stopLatch";
import type { QueuedSend } from "@/lib/sendQueue";
import type { ChatMessage } from "@/lib/session";
import { useSendQueue, type ExecuteSendFromQueue } from "@/hooks/useSendQueue";
import { createT } from "@/i18n";
import {
  useSessionTurnState,
} from "./session-turn/useSessionTurnState";
import { useSessionConnection } from "./session-turn/useSessionConnection";
import { useSessionSend } from "./session-turn/useSessionSend";
import { useSessionDraftSend } from "./session-turn/useSessionDraftSend";
import { useSessionEditResend } from "./session-turn/useSessionEditResend";
import { useSessionQueueSteering } from "./session-turn/useSessionQueueSteering";
import { useSessionStop } from "./session-turn/useSessionStop";
import type {
  EnsureConnected,
  ExecuteSend,
  Ref,
  SessionTurnStateRefs,
  UseSessionTurnOptions,
} from "./session-turn/types";

export type {
  EnsureConnected,
  ExecuteSend,
  ExecuteSendOptions,
  RetryStatus,
  SessionTurnApiPort,
  SessionTurnRuntimePort,
  SessionTurnStateRefs,
  SessionTurnUiPort,
  StreamStallState,
  UseSessionTurnOptions,
} from "./session-turn/types";

export interface SessionTurnResult {
  ensureConnected: EnsureConnected;
  executeSend: ExecuteSend;
  send: () => Promise<void>;
  sendRef: Ref<(() => Promise<void>) | null>;
  editAndResend: (
    message: ChatMessage,
    content: string,
  ) => Promise<boolean>;
  steerQueuedItem: (item: QueuedSend) => Promise<void>;
  stop: () => Promise<void>;
  connecting: boolean;
  connectingRef: Ref<boolean>;
  stopLatch: StopLatchState;
  stopLatchRef: Ref<StopLatchState>;
  sendInFlightRef: SessionTurnStateRefs["sendInFlightRef"];
  executeSendFromQueueRef: Ref<ExecuteSendFromQueue>;
  turnLatencyBySessionRef: SessionTurnStateRefs["turnLatencyBySessionRef"];
  activeTurnIdBySessionRef: SessionTurnStateRefs["activeTurnIdBySessionRef"];
  recoverableCompletedTurnIdBySessionRef: SessionTurnStateRefs["recoverableCompletedTurnIdBySessionRef"];
  completedTurnIdBySessionRef: SessionTurnStateRefs["completedTurnIdBySessionRef"];
  pendingVisibleTurnBySessionRef: SessionTurnStateRefs["pendingVisibleTurnBySessionRef"];
  observeHostActiveTurn: SessionTurnStateRefs["observeHostActiveTurn"];
  sendQueue: ReturnType<typeof useSendQueue>;
  queuePreviewLabels: {
    filesCount: (count: number) => string;
    empty: string;
  };
  effectiveCanSend: boolean;
  effectiveCanStop: boolean;
}

/** 组合回合领域的独立流程，保留 App 使用的单一公开契约。 */
export function useSessionTurn({
  locale,
  session,
  activeProject,
  draft,
  attachments,
  modelLabel,
  effort,
  hasConfiguredModel,
  goalModeSessionKey,
  planModeSessionKey,
  ultraModeSessionKey,
  api,
  runtime,
  ui,
  stateRefs,
  showToast,
}: UseSessionTurnOptions): SessionTurnResult {
  const tr = useMemo(() => createT(locale), [locale]);
  const state = useSessionTurnState(stateRefs);
  const ensureConnected = useSessionConnection({
    locale,
    tr,
    sessionId: session.sessionId,
    activeProject,
    effort,
    api,
    runtime,
    ui,
    state,
  });

  const sendQueueLabels = useMemo(
    () => ({
      queued: tr("composer.queued"),
      sendFailed: tr("composer.queueSendFailed"),
      droppedOldest: (n: number, max: number) =>
        tr("composer.queueDroppedOldest", {
          n: String(n),
          max: String(max),
        }),
    }),
    [tr],
  );
  const queuePreviewLabels = useMemo(
    () => ({
      filesCount: (count: number) =>
        tr("composer.queueFilesCount", { n: String(count) }),
      empty: tr("composer.queueEmptyPreview"),
    }),
    [tr],
  );
  const executeSendFromQueueRef = useRef<ExecuteSendFromQueue>(
    async () => false,
  );
  const sendQueue = useSendQueue({
    sessionId: session.sessionId,
    sessionState: session.state,
    connecting: state.connecting,
    liveHostRef: runtime.liveHostRef,
    viewingSessionIdRef: runtime.viewingSessionIdRef,
    sendInFlightRef: state.sendInFlightRef,
    executeSendRef: executeSendFromQueueRef,
    showToast,
    labels: sendQueueLabels,
  });

  const executeSend = useSessionSend({
    locale,
    tr,
    sessionId: session.sessionId,
    modelLabel,
    hasConfiguredModel,
    api,
    runtime,
    ui,
    state,
    ensureConnected,
    sendQueue,
  });
  executeSendFromQueueRef.current = executeSend;

  const { send, sendRef } = useSessionDraftSend({
    sessionId: session.sessionId,
    sessionState: session.state,
    connecting: state.connecting,
    draft,
    attachments,
    hasConfiguredModel,
    goalModeSessionKey,
    planModeSessionKey,
    ultraModeSessionKey,
    executeSend,
    sendQueue,
    ui,
  });
  const editAndResend = useSessionEditResend({
    locale,
    session,
    planModeSessionKey,
    ultraModeSessionKey,
    api,
    runtime,
    ui,
    state,
    executeSend,
  });
  const steerQueuedItem = useSessionQueueSteering({
    tr,
    sessionId: session.sessionId,
    sessionState: session.state,
    api,
    runtime,
    showToast,
  });
  const { stop, stopLatch, stopLatchRef } = useSessionStop({
    locale,
    api,
    runtime,
    ui,
    activeTurnIdBySessionRef: state.activeTurnIdBySessionRef,
  });

  return {
    ensureConnected,
    executeSend,
    send,
    sendRef,
    editAndResend,
    steerQueuedItem,
    stop,
    connecting: state.connecting,
    connectingRef: state.connectingRef,
    stopLatch,
    stopLatchRef,
    sendInFlightRef: state.sendInFlightRef,
    executeSendFromQueueRef,
    turnLatencyBySessionRef: state.turnLatencyBySessionRef,
    activeTurnIdBySessionRef: state.activeTurnIdBySessionRef,
    recoverableCompletedTurnIdBySessionRef:
      state.recoverableCompletedTurnIdBySessionRef,
    completedTurnIdBySessionRef: state.completedTurnIdBySessionRef,
    pendingVisibleTurnBySessionRef: state.pendingVisibleTurnBySessionRef,
    observeHostActiveTurn: state.observeHostActiveTurn,
    sendQueue,
    queuePreviewLabels,
    effectiveCanSend: canSendWithStopLatch(session.state, stopLatch),
    effectiveCanStop: canStopWithStopLatch(session.state, stopLatch),
  };
}

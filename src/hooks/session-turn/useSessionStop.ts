import { useCallback, useRef, useState } from "react";
import type { Locale } from "@/i18n";
import { localizeUiError, applyTurnMarker } from "@/lib/session";
import { endOfTurnMarkerContent } from "@/lib/endOfTurn";
import {
  armStopLatch,
  createStopLatchState,
  tickStopLatch,
  STOP_LATCH_MS,
  type StopLatchState,
} from "@/lib/stopLatch";
import type {
  Ref,
  SessionTurnApiPort,
  SessionTurnRuntimePort,
  SessionTurnState,
  SessionTurnUiPort,
} from "./types";

export interface UseSessionStopOptions {
  locale: Locale;
  api: SessionTurnApiPort;
  runtime: Pick<
    SessionTurnRuntimePort,
    | "acpWorkspaceRef"
    | "liveHostRef"
    | "viewingSessionIdRef"
    | "patchSessionMessages"
  >;
  ui: Pick<
    SessionTurnUiPort,
    | "setRetryStatus"
    | "setStreamStall"
    | "setTurnStartedAt"
    | "setLocalError"
    | "setMessages"
    | "setAskUser"
  >;
  activeTurnIdBySessionRef: SessionTurnState["activeTurnIdBySessionRef"];
  clearPendingAskUser: (sessionId?: string | null) => void;
}

export interface SessionStopResult {
  stop: () => Promise<void>;
  stopLatch: StopLatchState;
  stopLatchRef: Ref<StopLatchState>;
}

export function useSessionStop({
  locale,
  api,
  runtime,
  ui,
  activeTurnIdBySessionRef,
  clearPendingAskUser,
}: UseSessionStopOptions): SessionStopResult {
  const {
    acpWorkspaceRef,
    liveHostRef,
    viewingSessionIdRef,
    patchSessionMessages,
  } = runtime;
  const {
    setRetryStatus,
    setStreamStall,
    setTurnStartedAt,
    setLocalError,
    setMessages,
    setAskUser,
  } = ui;
  const stopLatchRef = useRef<StopLatchState>(createStopLatchState());
  const [stopLatch, setStopLatch] = useState<StopLatchState>(() =>
    createStopLatchState(),
  );

  const updateStopLatch = useCallback((next: StopLatchState) => {
    stopLatchRef.current = next;
    setStopLatch(next);
  }, []);

  const stop = useCallback(async () => {
    const now = Date.now();
    const sid =
      viewingSessionIdRef.current || liveHostRef.current.sessionId || null;
    const clearStoppedSessionRetry = () => {
      if (sid) {
        const view = acpWorkspaceRef.current.sessions[sid];
        if (view) view.retry = null;
      }
      setRetryStatus(null);
    };
    updateStopLatch(armStopLatch(stopLatchRef.current, sid, now));
    window.setTimeout(() => {
      const tick = tickStopLatch(
        stopLatchRef.current,
        liveHostRef.current.state,
        Date.now(),
        STOP_LATCH_MS,
      );
      updateStopLatch(tick.latch);
      if (tick.forceComplete) {
        const id = sid || liveHostRef.current.sessionId;
        if (id) {
          patchSessionMessages(id, (previous) =>
            applyTurnMarker(previous, {
              sessionId: id,
              messageId: `end-stop-${Date.now()}`,
              marker: "turn_end",
              reason: "user_stop",
              content: endOfTurnMarkerContent("user_stop"),
            }),
          );
          patchSessionMessages(id, (messages) =>
            messages.map((message) => ({ ...message, streaming: false })),
          );
        }
        clearStoppedSessionRetry();
        setStreamStall(null);
        setTurnStartedAt(null);
      }
    }, STOP_LATCH_MS + 50);
    try {
      const activeRequestId = sid
        ? activeTurnIdBySessionRef.current.get(sid)
        : null;
      if (sid && !activeRequestId) {
        throw new Error("当前运行回合缺少 requestId，无法安全停止");
      }
      if (sid && activeRequestId) await api.stop(sid, activeRequestId);
      clearStoppedSessionRetry();
      setStreamStall(null);
      setTurnStartedAt(null);
      const liveId = sid || liveHostRef.current.sessionId;
      if (liveId) {
        patchSessionMessages(liveId, (messages) =>
          messages.map((message) => ({ ...message, streaming: false })),
        );
        if (stopLatchRef.current.phase !== "force_idle") {
          patchSessionMessages(liveId, (messages) => {
            if (
              messages.some(
                (message) =>
                  message.marker === "turn_end" ||
                  message.marker === "turn_cancelled" ||
                  message.content?.startsWith("turn_end|"),
              )
            ) {
              return messages;
            }
            return applyTurnMarker(messages, {
              sessionId: liveId,
              messageId: `end-stop-ok-${Date.now()}`,
              marker: "turn_end",
              reason: "user_stop",
              content: endOfTurnMarkerContent("user_stop"),
            });
          });
        }
      } else {
        setMessages((messages) =>
          messages.map((message) => ({ ...message, streaming: false })),
        );
      }
      updateStopLatch(createStopLatchState());
    } catch (cause) {
      setLocalError(localizeUiError(cause, locale));
    } finally {
      if (sid) {
        clearPendingAskUser(sid);
        setAskUser((current) =>
          current?.sessionId === sid ? null : current,
        );
      }
    }
  }, [
    acpWorkspaceRef,
    activeTurnIdBySessionRef,
    api,
    clearPendingAskUser,
    liveHostRef,
    locale,
    patchSessionMessages,
    setAskUser,
    setLocalError,
    setMessages,
    setRetryStatus,
    setStreamStall,
    setTurnStartedAt,
    updateStopLatch,
    viewingSessionIdRef,
  ]);

  return { stop, stopLatch, stopLatchRef };
}

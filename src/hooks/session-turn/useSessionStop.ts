import { useCallback, useEffect, useRef, useState } from "react";
import type { Locale } from "@/i18n";
import { localizeUiError } from "@/lib/session";
import {
  armStopLatch,
  createStopLatchState,
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
  >;
  ui: Pick<
    SessionTurnUiPort,
    | "setRetryStatus"
    | "setStreamStall"
    | "setLocalError"
  >;
  activeTurnIdBySessionRef: SessionTurnState["activeTurnIdBySessionRef"];
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
}: UseSessionStopOptions): SessionStopResult {
  const {
    acpWorkspaceRef,
    liveHostRef,
    viewingSessionIdRef,
  } = runtime;
  const {
    setRetryStatus,
    setStreamStall,
    setLocalError,
  } = ui;
  const stopLatchRef = useRef<StopLatchState>(createStopLatchState());
  const [stopLatch, setStopLatch] = useState<StopLatchState>(() =>
    createStopLatchState(),
  );
  const stopTimerRef = useRef<number | null>(null);
  const stopAttemptRef = useRef(0);
  const stopRequestIdRef = useRef<string | null>(null);
  const stopTimedOutRef = useRef(false);

  const updateStopLatch = useCallback((next: StopLatchState) => {
    stopLatchRef.current = next;
    setStopLatch(next);
  }, []);

  const clearStopTimer = useCallback(() => {
    if (stopTimerRef.current == null) return;
    window.clearTimeout(stopTimerRef.current);
    stopTimerRef.current = null;
  }, []);

  /** 使旧 Stop 尝试失效并释放等待锁；不触碰当前回合的业务状态。 */
  const invalidateStopAttempt = useCallback(() => {
    stopAttemptRef.current += 1;
    stopRequestIdRef.current = null;
    stopTimedOutRef.current = false;
    clearStopTimer();
    updateStopLatch(createStopLatchState());
  }, [clearStopTimer, updateStopLatch]);

  /**
   * Stop 请求的完成边界只能来自 Host 的终态事件，并且必须先确认仍对应
   * Stop 时捕获的 active turn。新 Turn 已登记时，旧 Stop 绝不能收口新 Turn。
   */
  const isStopTargetSettled = useCallback(
    (sessionId: string, requestId: string): boolean => {
      const activeRequestId = activeTurnIdBySessionRef.current.get(sessionId);
      const activeRequestMatches = activeRequestId === requestId;
      if (activeRequestMatches || activeRequestId != null) return false;

      const liveHost = liveHostRef.current;
      if (liveHost.sessionId === sessionId) {
        return liveHost.state !== "streaming" && liveHost.state !== "connecting";
      }
      const view = acpWorkspaceRef.current.sessions[sessionId];
      return view != null && view.status !== "streaming";
    },
    [acpWorkspaceRef, activeTurnIdBySessionRef, liveHostRef],
  );

  /** 组件重渲染时只同步 latch；不修改消息或伪造任何回合终态。 */
  useEffect(() => {
    const latch = stopLatchRef.current;
    const requestId = stopRequestIdRef.current;
    if (latch.phase !== "waiting" || !latch.sessionId || !requestId) {
      return;
    }
    const activeRequestId = activeTurnIdBySessionRef.current.get(
      latch.sessionId,
    );
    if (activeRequestId != null && activeRequestId !== requestId) {
      invalidateStopAttempt();
      return;
    }
    if (isStopTargetSettled(latch.sessionId, requestId)) {
      invalidateStopAttempt();
    }
  });

  /** 仅清理计时器；Host 终态仍由 ACP 事件归约负责。 */
  useEffect(() => clearStopTimer, [clearStopTimer]);

  /** 超时只提示等待状态；不能把通知送达误报为回合完成。 */
  const stopPendingMessage =
    locale === "en"
      ? "Stop has not been confirmed; you can retry."
      : locale === "zh-TW"
        ? "尚未確認停止，可重試"
        : "尚未确认停止，可重试";

  const stop = useCallback(async () => {
    const sid =
      viewingSessionIdRef.current || liveHostRef.current.sessionId || null;
    if (!sid) return;

    const activeRequestId = activeTurnIdBySessionRef.current.get(sid);
    if (!activeRequestId) {
      setLocalError("当前运行回合缺少 requestId，无法安全停止");
      return;
    }

    // 同一 Session 的 stop 通知仍在等待时，忽略重复点击；超时后允许重试。
    if (
      stopLatchRef.current.phase === "waiting" &&
      stopLatchRef.current.sessionId === sid &&
      stopRequestIdRef.current === activeRequestId &&
      !stopTimedOutRef.current
    ) {
      return;
    }

    const attempt = stopAttemptRef.current + 1;
    stopAttemptRef.current = attempt;
    stopRequestIdRef.current = activeRequestId;
    stopTimedOutRef.current = false;
    clearStopTimer();
    updateStopLatch(armStopLatch(stopLatchRef.current, sid, Date.now()));

    // 预算只用于等待反馈，不会改写回合状态。
    stopTimerRef.current = window.setTimeout(() => {
      if (stopAttemptRef.current !== attempt) return;
      stopTimerRef.current = null;
      const latch = stopLatchRef.current;
      if (
        latch.phase !== "waiting" ||
        latch.sessionId !== sid ||
        stopRequestIdRef.current !== activeRequestId
      ) {
        return;
      }
      const currentRequestId = activeTurnIdBySessionRef.current.get(sid);
      if (currentRequestId != null && currentRequestId !== activeRequestId) {
        invalidateStopAttempt();
        return;
      }
      if (isStopTargetSettled(sid, activeRequestId)) {
        invalidateStopAttempt();
        return;
      }
      stopTimedOutRef.current = true;
      if (viewingSessionIdRef.current === sid) {
        setLocalError(stopPendingMessage);
      }
      // 保持 waiting，直到真实 TurnCancelled/TurnStopped（或等价的 ACP
      // agent-done）事件到达；这里不能清 streaming 或 turnStartedAt。
      updateStopLatch({ ...latch, phase: "waiting" });
    }, STOP_LATCH_MS + 50);

    try {
      await api.stop(sid, activeRequestId);
      if (stopAttemptRef.current !== attempt) return;
      const currentRequestId = activeTurnIdBySessionRef.current.get(sid);
      if (currentRequestId != null && currentRequestId !== activeRequestId) {
        invalidateStopAttempt();
        return;
      }
      if (
        viewingSessionIdRef.current === sid &&
        currentRequestId === activeRequestId
      ) {
        setRetryStatus(null);
        setStreamStall(null);
      }
      if (isStopTargetSettled(sid, activeRequestId)) {
        invalidateStopAttempt();
      }
    } catch (cause) {
      if (stopAttemptRef.current !== attempt) return;
      const currentRequestId = activeTurnIdBySessionRef.current.get(sid);
      if (currentRequestId != null && currentRequestId !== activeRequestId) {
        invalidateStopAttempt();
        return;
      }
      invalidateStopAttempt();
      setLocalError(localizeUiError(cause, locale));
    }
  }, [
    activeTurnIdBySessionRef,
    api,
    clearStopTimer,
    invalidateStopAttempt,
    isStopTargetSettled,
    liveHostRef,
    locale,
    setLocalError,
    setRetryStatus,
    setStreamStall,
    stopPendingMessage,
    updateStopLatch,
    viewingSessionIdRef,
  ]);

  return { stop, stopLatch, stopLatchRef };
}

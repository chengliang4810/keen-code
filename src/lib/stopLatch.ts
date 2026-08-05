/**
 * Stop / interrupt honesty — if Host does not leave busy within a budget,
 * UI force-unlocks the composer (CodePilot-style force-abort window).
 */

import type { SessionState } from "./session";
import { canSend, isSessionLiveStreaming } from "./session";

/** Default wait after user Stop before force-idle (ms). */
export const STOP_LATCH_MS = 2000;

export type StopLatchPhase = "idle" | "waiting" | "force_idle";

export interface StopLatchState {
  phase: StopLatchPhase;
  /** sessionId the latch was started for */
  sessionId: string | null;
  /** epoch ms when Stop was requested */
  startedAt: number | null;
}

export function createStopLatchState(): StopLatchState {
  return { phase: "idle", sessionId: null, startedAt: null };
}

/** User hit Stop — arm the latch. */
export function armStopLatch(
  _prev: StopLatchState,
  sessionId: string | null,
  nowMs: number,
): StopLatchState {
  return {
    phase: "waiting",
    sessionId: sessionId ?? null,
    startedAt: nowMs,
  };
}

/**
 * Tick the latch against Host state.
 * Returns next latch + whether UI should force-complete the turn.
 */
export function tickStopLatch(
  latch: StopLatchState,
  hostState: SessionState,
  nowMs: number,
  budgetMs: number = STOP_LATCH_MS,
): { latch: StopLatchState; forceComplete: boolean } {
  if (latch.phase === "idle") {
    return { latch, forceComplete: false };
  }
  // Host already idle → clear.
  if (!isSessionLiveStreaming(hostState) && canSend(hostState)) {
    return { latch: createStopLatchState(), forceComplete: false };
  }
  if (latch.phase === "waiting" && latch.startedAt != null) {
    if (nowMs - latch.startedAt >= budgetMs) {
      return {
        latch: {
          ...latch,
          phase: "force_idle",
        },
        forceComplete: true,
      };
    }
  }
  // Already force_idle — keep until host clears or new arm.
  if (latch.phase === "force_idle") {
    return { latch, forceComplete: false };
  }
  return { latch, forceComplete: false };
}

/**
 * Effective canSend while a force latch is active.
 * When force_idle, treat as sendable even if Host still says streaming.
 */
export function canSendWithStopLatch(
  hostState: SessionState,
  latch: StopLatchState,
): boolean {
  if (latch.phase === "force_idle") return true;
  return canSend(hostState);
}

export function canStopWithStopLatch(
  hostState: SessionState,
  latch: StopLatchState,
): boolean {
  if (latch.phase === "force_idle") return false;
  return hostState === "streaming";
}

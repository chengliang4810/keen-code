/** Stop / interrupt state — waiting feedback never overrides Host state. */

import type { SessionState } from "./session";
import { canSend } from "./session";

/** Default wait before showing that Stop has not been confirmed (ms). */
export const STOP_LATCH_MS = 2000;

export type StopLatchPhase = "idle" | "waiting";

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

/** Waiting Stop feedback never makes a still-streaming Host sendable. */
export function canSendWithStopLatch(
  hostState: SessionState,
  _latch: StopLatchState,
): boolean {
  return canSend(hostState);
}

export function canStopWithStopLatch(
  hostState: SessionState,
  _latch: StopLatchState,
): boolean {
  return hostState === "streaming";
}

import { describe, expect, it } from "vitest";
import {
  STOP_LATCH_MS,
  armStopLatch,
  canSendWithStopLatch,
  createStopLatchState,
  tickStopLatch,
} from "./stopLatch";

describe("stopLatch", () => {
  it("force-completes after budget while still streaming", () => {
    let latch = createStopLatchState();
    latch = armStopLatch(latch, "s1", 1000);
    expect(canSendWithStopLatch("streaming", latch)).toBe(false);
    let r = tickStopLatch(latch, "streaming", 1000 + STOP_LATCH_MS - 1);
    expect(r.forceComplete).toBe(false);
    r = tickStopLatch(latch, "streaming", 1000 + STOP_LATCH_MS);
    expect(r.forceComplete).toBe(true);
    expect(r.latch.phase).toBe("force_idle");
    expect(canSendWithStopLatch("streaming", r.latch)).toBe(true);
  });

  it("clears when host becomes ready", () => {
    let latch = armStopLatch(createStopLatchState(), "s1", 1000);
    const r = tickStopLatch(latch, "ready", 1100);
    expect(r.latch.phase).toBe("idle");
    expect(r.forceComplete).toBe(false);
  });
});

import { describe, expect, it } from "vitest";
import {
  armStopLatch,
  canSendWithStopLatch,
  canStopWithStopLatch,
  createStopLatchState,
} from "./stopLatch";

describe("stopLatch", () => {
  it("waiting does not override streaming Host state", () => {
    let latch = createStopLatchState();
    latch = armStopLatch(latch, "s1", 1000);
    expect(canSendWithStopLatch("streaming", latch)).toBe(false);
    expect(canStopWithStopLatch("streaming", latch)).toBe(true);
    expect(latch.phase).toBe("waiting");
  });

  it("ready Host state controls send and stop independently of latch", () => {
    let latch = armStopLatch(createStopLatchState(), "s1", 1000);
    expect(canSendWithStopLatch("ready", latch)).toBe(true);
    expect(canStopWithStopLatch("ready", latch)).toBe(false);
    expect(latch.phase).toBe("waiting");
  });
});

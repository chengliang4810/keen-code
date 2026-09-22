import { describe, expect, it, vi } from "vitest";
import { createAnimationFrameBatcher } from "./frameBatcher";

describe("createAnimationFrameBatcher", () => {
  it("同一动画帧只发布一次", () => {
    const callbacks: FrameRequestCallback[] = [];
    const publish = vi.fn();
    const requestFrame = vi.fn((next: FrameRequestCallback) => {
      callbacks.push(next);
      return 7;
    });
    const batcher = createAnimationFrameBatcher(
      publish,
      requestFrame,
      vi.fn(),
    );

    batcher.schedule();
    batcher.schedule();

    expect(requestFrame).toHaveBeenCalledTimes(1);
    expect(publish).not.toHaveBeenCalled();
    callbacks[0]!(16);
    expect(publish).toHaveBeenCalledTimes(1);
  });

  it("语义边界取消待处理帧并同步发布", () => {
    const publish = vi.fn();
    const cancelFrame = vi.fn();
    const batcher = createAnimationFrameBatcher(
      publish,
      () => 9,
      cancelFrame,
    );

    batcher.schedule();
    batcher.flush();

    expect(cancelFrame).toHaveBeenCalledWith(9);
    expect(publish).toHaveBeenCalledTimes(1);
  });
});

type RequestFrame = (callback: FrameRequestCallback) => number;
type CancelFrame = (id: number) => void;

export interface AnimationFrameBatcher {
  /** 合并同一绘制帧内的高频发布。 */
  schedule(): void;
  /** 取消待处理帧并立即发布一次边界状态。 */
  flush(): void;
  /** 卸载时取消，且不发布。 */
  cancel(): void;
}

/**
 * 把高频状态投影合并到浏览器绘制边界；工具、错误和完成等语义边界可同步 flush。
 */
export function createAnimationFrameBatcher(
  publish: () => void,
  requestFrame: RequestFrame,
  cancelFrame: CancelFrame,
): AnimationFrameBatcher {
  let frameId: number | null = null;

  return {
    schedule() {
      if (frameId !== null) return;
      frameId = requestFrame(() => {
        frameId = null;
        publish();
      });
    },
    flush() {
      if (frameId !== null) {
        cancelFrame(frameId);
        frameId = null;
      }
      publish();
    },
    cancel() {
      if (frameId === null) return;
      cancelFrame(frameId);
      frameId = null;
    },
  };
}

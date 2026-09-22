/** 将输入区实测高度归一化为稳定的整数像素，避免 ResizeObserver 抖动。 */
export function normalizeComposerHeight(composerHeight: number): number {
  return Math.ceil(Math.max(0, Number.isFinite(composerHeight) ? composerHeight : 0));
}

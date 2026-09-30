/**
 * markdown 解析耗时埋点。
 *
 * 口径：单次 remark `processor.parse` 的真实耗时。streamdown 按 props 键把
 * unified processor 缓存在 LRU 中，插件 attacher 只在缓存未命中时运行一次；
 * 若沿用"attacher 记起点、transformer 记终点"的旧口径，时长会随 processor
 * 复用膨胀为该块的整个流式生命周期（观测值可达分钟级）。因此起点取自包装
 * 后的 parse，transformer 只负责在每次解析完成后上报，与复用次数无关。
 */

import type { Processor } from "unified";
import { recordMarkdownParse } from "@/lib/frontendPerformance";

/** 同一 processor 只注入一次测量钩子，防止重复包装导致重复上报。 */
const measuredProcessors = new WeakSet<Processor>();

export function createMeasureRemarkPlugin(turnId: string, fullParse: boolean) {
  return function measurePlugin(this: Processor) {
    const processor = this;
    if (measuredProcessors.has(processor)) {
      return () => {};
    }
    measuredProcessors.add(processor);
    let parseDurationMs = 0;
    const originalParse = processor.parse.bind(processor);
    processor.parse = (file, ...rest) => {
      const started =
        typeof performance === "undefined" ? 0 : performance.now();
      const tree = originalParse(file, ...rest);
      parseDurationMs =
        typeof performance === "undefined"
          ? 0
          : performance.now() - started;
      return tree;
    };
    return (_tree: unknown, file: { value?: unknown }) => {
      recordMarkdownParse({
        turnId,
        durationMs: parseDurationMs,
        sourceChars: String(file?.value ?? "").length,
        fullParse,
      });
    };
  };
}

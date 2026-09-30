import { describe, expect, it, vi } from "vitest";
import type { Processor } from "unified";

type MarkdownParseRecord = {
  turnId: string;
  durationMs: number;
  sourceChars: number;
  fullParse: boolean;
};

const markdownParseRecords = vi.hoisted(() => [] as MarkdownParseRecord[]);

vi.mock("@/lib/frontendPerformance", () => ({
  recordMarkdownParse: (input: MarkdownParseRecord) => {
    markdownParseRecords.push(input);
  },
}));

import { createMeasureRemarkPlugin } from "./markdownMeasure";

function fakeProcessor(parse: () => unknown): Processor {
  return { parse } as unknown as Processor;
}

/** 顺序驱动 performance.now()：第 n 次调用返回 base + n * step。 */
function mockPerformanceNow(base: number, step: number) {
  let calls = 0;
  return vi
    .spyOn(performance, "now")
    .mockImplementation(() => base + (calls += 1) * step);
}

describe("createMeasureRemarkPlugin", () => {
  it("包装 parse 并上报单次解析的真实耗时", () => {
    markdownParseRecords.length = 0;
    const parse = vi.fn(() => "tree");
    const now = mockPerformanceNow(1_000, 50);
    const plugin = createMeasureRemarkPlugin("turn-1", false);
    const processor = fakeProcessor(parse);
    const transformer = plugin.call(processor);

    const tree = processor.parse();
    transformer(undefined, { value: "abc" });

    expect(tree).toBe("tree");
    expect(parse).toHaveBeenCalledTimes(1);
    // parse 起点 1050、终点 1100 → 50ms；transformer 不再引入挂载起点。
    expect(markdownParseRecords).toEqual([
      { turnId: "turn-1", durationMs: 50, sourceChars: 3, fullParse: false },
    ]);
    now.mockRestore();
  });

  it("processor 复用时每次 parse 都上报新耗时，不随时间膨胀", () => {
    markdownParseRecords.length = 0;
    const processor = fakeProcessor(() => "tree");
    const now = mockPerformanceNow(1_000, 1_000);
    const plugin = createMeasureRemarkPlugin("turn-1", false);
    const transformer = plugin.call(processor);

    processor.parse();
    transformer(undefined, { value: "a" });
    // 第二次 parse 发生在很久之后；旧口径会报出累计时长。
    processor.parse();
    transformer(undefined, { value: "a" });

    expect(markdownParseRecords.map((record) => record.durationMs)).toEqual([
      1_000, 1_000,
    ]);
    now.mockRestore();
  });

  it("同一 processor 重复注入时不再包装，上报只有一份", () => {
    markdownParseRecords.length = 0;
    const parse = vi.fn(() => "tree");
    const now = mockPerformanceNow(1_000, 10);
    const plugin = createMeasureRemarkPlugin("turn-1", true);
    const processor = fakeProcessor(parse);
    const first = plugin.call(processor);
    const second = plugin.call(processor);

    processor.parse();
    first(undefined, { value: "x" });
    second(undefined, { value: "x" });

    expect(parse).toHaveBeenCalledTimes(1);
    expect(markdownParseRecords).toEqual([
      { turnId: "turn-1", durationMs: 10, sourceChars: 1, fullParse: true },
    ]);
    now.mockRestore();
  });
});

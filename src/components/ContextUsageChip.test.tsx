import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ContextUsageChip,
  formatContextUsagePercentage,
  formatTaskCacheHitRate,
} from "./ContextUsageChip";

describe("ContextUsageChip", () => {
  it("使用圆环展示占用，并把详细用量保留在悬浮提示与无障碍名称中", () => {
    const html = renderToString(
      <ContextUsageChip
        display={{
          tokens: 32_000,
          source: "known",
          label: "32k / 128k",
          contextWindow: 128_000,
          percentage: 25,
        }}
        taskCacheUsage={{
          sessionId: "session-1",
          requestCount: 3,
          inputTokens: 40_000,
          cacheReadTokens: 30_000,
          cacheHitRate: 0.75,
        }}
        labels={{
          aria: "上下文用量",
          contextUsageRate: "上下文使用率",
          taskCacheHitRate: "任务缓存命中率",
        }}
      />,
    );

    expect(html).toContain("context-ring");
    expect(html).toContain('width="16"');
    expect(html).toContain("上下文用量: 32k / 128k");
    expect(html).toContain("上下文使用率: 25%");
    expect(html).toContain("任务缓存命中率: 75%");
    expect(html).not.toContain("chip__label--nums");
  });

  it("格式化上下文使用率并区分未知值", () => {
    expect(formatContextUsagePercentage(0)).toBe("0%");
    expect(formatContextUsagePercentage(0.9375)).toBe("0.9%");
    expect(formatContextUsagePercentage(0.9375, "estimated")).toBe("~0.9%");
    expect(formatContextUsagePercentage(25)).toBe("25%");
    expect(formatContextUsagePercentage(99.96)).toBe("100%");
    expect(formatContextUsagePercentage(null)).toBe("—");
    expect(formatContextUsagePercentage(25, "unknown")).toBe("—");
    expect(formatContextUsagePercentage(-0.1)).toBe("—");
    expect(formatContextUsagePercentage(100.1)).toBe("—");
  });

  it("区分任务明确零命中、未知和非法比例", () => {
    expect(formatTaskCacheHitRate(0)).toBe("0%");
    expect(formatTaskCacheHitRate(0.994)).toBe("99.4%");
    expect(formatTaskCacheHitRate(1)).toBe("100%");
    expect(formatTaskCacheHitRate(null)).toBe("—");
    expect(formatTaskCacheHitRate(-0.1)).toBe("—");
    expect(formatTaskCacheHitRate(1.01)).toBe("—");
  });
});

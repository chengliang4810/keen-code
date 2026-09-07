import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { createTurnLatencyState, summarizeTurnLatency } from "@/lib/turnLatency";
import { formatMetricTokens, formatRunDuration, formatTurnLatency, formatTokensPerSecond } from "@/lib/turnMetricsPresentation";
import { TurnMetrics } from "./TurnMetrics";

describe("TurnMetrics", () => {
  it("按 Harness 的整数秒和 K/M 规则显示两个独立明细入口", () => {
    const summary = { ...summarizeTurnLatency(createTurnLatencyState("turn-1", 0)),
      totalMs: 12_340, inputTokens: 10_000, outputTokens: 2_300, totalTokens: 12_300 };
    const html = renderToString(<TurnMetrics locale="zh" summary={summary} />);
    expect(html).toContain("用量 12.3K");
    expect(html).toContain("用时 12秒");
    expect(html.match(/aria-haspopup="dialog"/g)).toHaveLength(2);
    expect(html).toContain('aria-label="本轮用量"');
    expect(html).toContain('aria-label="本轮用时"');
    expect(html).not.toContain("发送确认");
  });
  it("缺失用量不伪造零，旧记录仍提供两个入口", () => {
    const html = renderToString(<TurnMetrics locale="zh-TW" durationMs={2_000} />);
    expect(html).toContain("用量 —");
    expect(html).toContain("用時 2秒");
    expect(html.match(/aria-haspopup="dialog"/g)).toHaveLength(2);
  });
  it("明确零与未报告分别显示", () => {
    const summary = { ...summarizeTurnLatency(createTurnLatencyState("turn-1", 0)), totalMs: 0, totalTokens: 0 };
    const html = renderToString(<TurnMetrics locale="en" summary={summary} />);
    expect(html).toContain("Usage 0");
    expect(html).toContain("Time 0s");
    expect(formatMetricTokens(null, "zh", true)).toBeNull();
    expect(formatMetricTokens(Number.MAX_SAFE_INTEGER + 1, "zh", false)).toBeNull();
  });
  it("时长和精确计数不将毫秒误当秒", () => {
    expect(formatTurnLatency(420)).toBe("420ms");
    expect(formatTurnLatency(1_250)).toBe("1.25s");
    expect(formatRunDuration(122_900, "zh")).toBe("2分02秒");
    expect(formatRunDuration(999, "zh")).toBe("0秒");
    expect(formatRunDuration(-1, "zh")).toBeNull();
    expect(formatMetricTokens(12_345, "en", false)).toBe("12,345");
    expect(formatMetricTokens(517_000, "en", true)).toBe("517K");
    expect(formatMetricTokens(1_234_000, "en", true)).toBe("1.2M");
  });
});


it("TPS 显示精度直接遵循 Harness", () => {
  expect(formatTokensPerSecond(135.5)).toBe("136");
  expect(formatTokensPerSecond(3.14)).toBe("3.1");
  expect(formatTokensPerSecond(0)).toBe("0");
  expect(formatTokensPerSecond(null)).toBeNull();
});

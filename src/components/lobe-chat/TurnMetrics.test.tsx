import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { TurnLatencySummary } from "@/lib/turnLatency";
import {
  formatCacheHitRate,
  formatTurnLatency,
  hasDisplayableTurnMetrics,
  TurnMetrics,
} from "./TurnMetrics";

function summary(
  patch: Partial<TurnLatencySummary> = {},
): TurnLatencySummary {
  return {
    turnId: "turn-1",
    sendAcknowledgementMs: null,
    timeToFirstSseMs: null,
    timeToFirstVisibleTokenMs: null,
    totalMs: null,
    inputTokens: null,
    cacheReadTokens: null,
    cacheCreationTokens: null,
    cacheHitRate: null,
    ...patch,
  };
}

describe("TurnMetrics", () => {
  it("以毫秒、秒和分秒展示紧凑耗时", () => {
    expect(formatTurnLatency(0)).toBe("0ms");
    expect(formatTurnLatency(420)).toBe("420ms");
    expect(formatTurnLatency(1_250)).toBe("1.25s");
    expect(formatTurnLatency(12_340)).toBe("12.3s");
    expect(formatTurnLatency(122_900)).toBe("2m 2s");
    expect(formatTurnLatency(-1)).toBeNull();
    expect(formatTurnLatency(Number.NaN)).toBeNull();
  });

  it("明确展示零命中并拒绝不可能的比例", () => {
    expect(formatCacheHitRate(0)).toBe("0%");
    expect(formatCacheHitRate(0.994)).toBe("99.4%");
    expect(formatCacheHitRate(1)).toBe("100%");
    expect(formatCacheHitRate(-0.1)).toBeNull();
    expect(formatCacheHitRate(1.01)).toBeNull();
  });

  it("按发送确认、首 SSE、首可见 Token、完成和缓存顺序展示", () => {
    const html = renderToString(
      <TurnMetrics
        locale="zh"
        summary={summary({
          sendAcknowledgementMs: 18,
          timeToFirstSseMs: 680,
          timeToFirstVisibleTokenMs: 735,
          totalMs: 12_340,
          inputTokens: 10_000,
          cacheReadTokens: 9_940,
          cacheCreationTokens: 0,
          cacheHitRate: 0.994,
        })}
      />,
    );

    const labels = [
      "发送确认 18ms",
      "首 SSE 680ms",
      "首可见 Token 735ms",
      "完成 12.3s",
      "缓存命中 99.4%",
    ];
    labels.forEach((label) => expect(html).toContain(label));
    labels.slice(1).forEach((label, index) => {
      expect(html.indexOf(labels[index]!)).toBeLessThan(html.indexOf(label));
    });
    expect(html).toContain('data-testid="turn-metrics"');
    expect(html).toContain('tabindex="0"');
    expect(html).toContain("本轮延迟与缓存命中率");
  });

  it("缺失指标逐项隐藏，但零缓存命中率仍可审查", () => {
    const partial = summary({
      totalMs: 2_000,
      inputTokens: 500,
      cacheReadTokens: 0,
      cacheHitRate: 0,
    });
    const html = renderToString(
      <TurnMetrics locale="zh-TW" summary={partial} />,
    );

    expect(hasDisplayableTurnMetrics(partial)).toBe(true);
    expect(html).toContain("完成 2s");
    expect(html).toContain("快取命中 0%");
    expect(html).not.toContain("傳送確認");
    expect(html).not.toContain("首 SSE");
    expect(html).not.toContain("首個可見 Token");
  });

  it("没有任何有效观测时不占用 footer", () => {
    const empty = summary();
    expect(hasDisplayableTurnMetrics(empty)).toBe(false);
    expect(renderToString(<TurnMetrics locale="en" summary={empty} />)).toBe(
      "",
    );
  });
});

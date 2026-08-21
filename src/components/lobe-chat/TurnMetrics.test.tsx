import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { TurnLatencySummary } from "@/lib/turnLatency";
import {
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

  it("按发送确认、首 SSE、首可见 Token 和完成顺序展示", () => {
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
        })}
      />,
    );

    const labels = [
      "发送确认 18ms",
      "首 SSE 680ms",
      "首可见 Token 735ms",
      "完成 12.3s",
    ];
    labels.forEach((label) => expect(html).toContain(label));
    labels.slice(1).forEach((label, index) => {
      expect(html.indexOf(labels[index]!)).toBeLessThan(html.indexOf(label));
    });
    expect(html).toContain('data-testid="turn-metrics"');
    expect(html).toContain('tabindex="0"');
    expect(html).toContain("本轮延迟");
    expect(html).not.toContain("缓存命中");
  });

  it("缺失延迟指标逐项隐藏，缓存率不再放在单轮 footer", () => {
    const partial = summary({
      totalMs: 2_000,
      inputTokens: 500,
      cacheReadTokens: 0,
    });
    const html = renderToString(
      <TurnMetrics locale="zh-TW" summary={partial} />,
    );

    expect(hasDisplayableTurnMetrics(partial)).toBe(true);
    expect(html).toContain("完成 2s");
    expect(html).not.toContain("快取命中");
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
    expect(hasDisplayableTurnMetrics(summary({ inputTokens: 500 }))).toBe(false);
  });
});

import { describe, expect, it } from "vitest";
import {
  analyticsModelPercent,
  analyticsTrendTickIndexes,
  buildAnalyticsTrendDays,
  formatAnalyticsTrendDate,
  localDateKey,
} from "./AnalyticsSettingsPanel";

describe("AnalyticsSettingsPanel token trend", () => {
  it("按本地日历补齐缺失日期，并让日期序列与数据序列保持一致", () => {
    const today = new Date(2026, 0, 3, 18, 30);
    const days = buildAnalyticsTrendDays(
      [
        { date: "2026-01-02", requests: 2, totalTokens: 20, modelTokens: { alpha: 20 } },
        { date: "2025-12-31", requests: 1, totalTokens: 10, modelTokens: { alpha: 10 } },
      ],
      5,
      today,
    );

    expect(days.map((item) => item.dateKey)).toEqual([
      "2025-12-30",
      "2025-12-31",
      "2026-01-01",
      "2026-01-02",
      "2026-01-03",
    ]);
    expect(days.map((item) => item.stat?.totalTokens ?? 0)).toEqual([0, 10, 0, 20, 0]);
    expect(days.map((item) => localDateKey(item.date))).toEqual(days.map((item) => item.dateKey));
    expect(formatAnalyticsTrendDate(days[1].date)).toBe("12/31");
  });

  it("为空、单点和长跨度选择不拥挤且包含首尾的可读刻度", () => {
    expect(analyticsTrendTickIndexes(0)).toEqual([]);
    expect(analyticsTrendTickIndexes(1)).toEqual([0]);
    expect(analyticsTrendTickIndexes(4)).toEqual([0, 1, 2, 3]);
    expect(analyticsTrendTickIndexes(31, 1)).toEqual([0]);
    expect(analyticsTrendTickIndexes(31)).toEqual([0, 5, 10, 15, 20, 25, 30]);
  });

  it("Provider 未报告 Token 时模型占比保持为有限的 0", () => {
    expect(analyticsModelPercent(0, 0)).toBe(0);
    expect(analyticsModelPercent(25, 100)).toBe(0.25);
  });
});

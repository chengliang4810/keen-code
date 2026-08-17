import { describe, expect, it } from "vitest";
import {
  formatAnalyticsCacheHitRate,
  formatAnalyticsRequestMode,
  formatRequestElapsed,
} from "./AnalyticsSettingsPanel";

describe("AnalyticsSettingsPanel observability", () => {
  it("区分未报告里程碑与零毫秒", () => {
    expect(formatRequestElapsed(1_000, null, "未报告")).toBe("未报告");
    expect(formatRequestElapsed(1_000, 1_000, "未报告")).toBe("0 ms");
    expect(formatRequestElapsed(1_000, 1_125, "未报告")).toBe("125 ms");
  });

  it("区分未报告缓存与明确零命中", () => {
    expect(formatAnalyticsCacheHitRate(null, "未报告")).toBe("未报告");
    expect(formatAnalyticsCacheHitRate(0, "未报告")).toBe("0%");
    expect(formatAnalyticsCacheHitRate(0.994, "未报告")).toBe("99.4%");
  });

  it("前台回合观测不会伪装成异步 LLM 请求", () => {
    const labels = { sync: "同步", async: "异步", turn: "回合" };
    expect(formatAnalyticsRequestMode("sync", labels)).toBe("同步");
    expect(formatAnalyticsRequestMode("async", labels)).toBe("异步");
    expect(formatAnalyticsRequestMode("turn", labels)).toBe("回合");
  });
});

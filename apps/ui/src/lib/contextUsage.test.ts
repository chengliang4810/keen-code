import { describe, expect, it } from "vitest";
import {
  attachContextWindow,
  formatContextChipLabel,
  formatTokenCount,
  invalidateSessionContextUsage,
} from "./contextUsage";

describe("formatTokenCount", () => {
  it("handles edge and scale bands", () => {
    expect(formatTokenCount(-1)).toBe("—");
    expect(formatTokenCount(NaN)).toBe("—");
    expect(formatTokenCount(0)).toBe("0");
    expect(formatTokenCount(42)).toBe("42");
    expect(formatTokenCount(999)).toBe("999");
    expect(formatTokenCount(1000)).toBe("1k");
    expect(formatTokenCount(1500)).toBe("1.5k");
    expect(formatTokenCount(10_000)).toBe("10k");
    expect(formatTokenCount(12_400)).toBe("12k");
    expect(formatTokenCount(1_000_000)).toBe("1M");
    expect(formatTokenCount(1_500_000)).toBe("1.5M");
  });
});

describe("formatContextChipLabel", () => {
  it("prefixes estimated values and uses em dash for unknown usage", () => {
    expect(formatContextChipLabel(null, "unknown")).toBe("—");
    expect(formatContextChipLabel(1200, "known")).toBe("1.2k");
    expect(formatContextChipLabel(1200, "estimated")).toBe("~1.2k");
  });
});

describe("invalidateSessionContextUsage", () => {
  it("只清除目标 Session 的缓存", () => {
    const usages = new Map([
      ["session-a", { used: 10, estimated: false }],
      ["session-b", { used: 20, estimated: true }],
    ]);
    invalidateSessionContextUsage(usages, "session-a");
    expect(usages).toEqual(new Map([
      ["session-b", { used: 20, estimated: true }],
    ]));
  });
});

describe("attachContextWindow", () => {
  it("adds a model window and clamps the ring percentage", () => {
    expect(
      attachContextWindow(
        { tokens: 1_200, source: "estimated", label: "~1.2k" },
        128_000,
      ),
    ).toEqual({
      tokens: 1_200,
      source: "estimated",
      label: "~1.2k / 128k",
      contextWindow: 128_000,
      percentage: 0.9375,
    });
    expect(
      attachContextWindow(
        { tokens: 200_000, source: "known", label: "200k" },
        128_000,
      ).percentage,
    ).toBe(100);
  });

  it("leaves unknown usage or invalid windows unchanged", () => {
    const unknown = { tokens: null, source: "unknown" as const, label: "—" };
    expect(attachContextWindow(unknown, 128_000)).toBe(unknown);
    const known = { tokens: 10, source: "known" as const, label: "10" };
    expect(attachContextWindow(known, 0)).toBe(known);
    expect(attachContextWindow(known, 0.5)).toBe(known);
  });
});

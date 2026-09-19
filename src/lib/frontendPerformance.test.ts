import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { performanceRecord } from "@/lib/acp/api";
import {
  beginFrontendTurnPerformance,
  completeFrontendTurnPerformance,
  recordFrontendDelivery,
  recordFrontendProjection,
  recordMarkdownParse,
  recordVirtualizerWork,
  resetFrontendPerformanceForTests,
} from "./frontendPerformance";

vi.mock("@/lib/acp/api", () => ({
  performanceRecord: vi.fn().mockResolvedValue(undefined),
}));

describe("frontendPerformance", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetFrontendPerformanceForTests();
    vi.mocked(performanceRecord).mockClear();
  });

  afterEach(() => {
    resetFrontendPerformanceForTests();
    vi.useRealTimers();
  });

  it("每个根 Turn 只写一条聚合性能记录", async () => {
    beginFrontendTurnPerformance("session-a", "turn-a");
    recordFrontendDelivery("session-a");
    recordFrontendDelivery("session-a");
    recordFrontendProjection("session-a", 3.25);
    recordMarkdownParse({
      turnId: "turn-a",
      durationMs: 4.75,
      sourceChars: 120,
      fullParse: false,
    });
    recordVirtualizerWork("session-a", 1.5, true);

    completeFrontendTurnPerformance("session-a", "turn-a");
    completeFrontendTurnPerformance("session-a", "turn-a");
    await vi.advanceTimersByTimeAsync(1_000);

    expect(performanceRecord).toHaveBeenCalledTimes(1);
    expect(performanceRecord).toHaveBeenCalledWith(
      "frontend.turn_performance",
      expect.any(String),
    );
    const payload = JSON.parse(vi.mocked(performanceRecord).mock.calls[0]![1]);
    expect(payload).toMatchObject({
      schema: 1,
      sessionId: "session-a",
      turnId: "turn-a",
      deliveryCount: 2,
      projection: { count: 1, totalMs: 3.3, maxMs: 3.3 },
      markdown: {
        count: 1,
        totalMs: 4.8,
        maxMs: 4.8,
        sourceChars: 120,
        maxSourceChars: 120,
        fullParseCount: 0,
      },
      virtualizer: {
        count: 1,
        totalMs: 1.5,
        maxMs: 1.5,
        measurementCount: 1,
      },
    });
  });

  it("新 Turn 不接收上一个 Turn 延迟到达的 Session 采样", async () => {
    beginFrontendTurnPerformance("session-a", "turn-a");
    completeFrontendTurnPerformance("session-a", "turn-a");
    beginFrontendTurnPerformance("session-a", "turn-b");
    recordFrontendDelivery("session-a");
    completeFrontendTurnPerformance("session-a", "turn-b");

    await vi.advanceTimersByTimeAsync(1_000);

    expect(performanceRecord).toHaveBeenCalledTimes(2);
    const payloads = vi.mocked(performanceRecord).mock.calls.map((call) =>
      JSON.parse(call[1]),
    );
    const previous = payloads.find((payload) => payload.turnId === "turn-a");
    const current = payloads.find((payload) => payload.turnId === "turn-b");
    expect(previous?.deliveryCount).toBe(0);
    expect(current?.deliveryCount).toBe(1);
  });
});

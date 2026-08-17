import { describe, expect, it } from "vitest";
import {
  createTurnLatencyState,
  reduceTurnLatency,
  summarizeTurnLatency,
  turnUsageActionFromAcp,
} from "./turnLatency";

describe("turn latency reducer", () => {
  it("用 null 区分尚未观测的里程碑和用量", () => {
    const state = createTurnLatencyState("turn-1", 1_000);

    expect(state).toMatchObject({
      startedAtMs: 1_000,
      sendAcknowledgedAtMs: null,
      firstSseAtMs: null,
      firstVisibleTokenAtMs: null,
      completedAtMs: null,
      usageObservations: [],
    });
    expect(summarizeTurnLatency(state)).toEqual({
      turnId: "turn-1",
      sendAcknowledgementMs: null,
      timeToFirstSseMs: null,
      timeToFirstVisibleTokenMs: null,
      totalMs: null,
      inputTokens: null,
      cacheReadTokens: null,
      cacheCreationTokens: null,
      cacheHitRate: null,
    });
  });

  it("记录发送确认、首 SSE、首个可见 Token 和完成耗时", () => {
    let state = createTurnLatencyState("turn-1", 10_000);
    state = reduceTurnLatency(state, {
      type: "send_acknowledged",
      turnId: "turn-1",
      atMs: 10_012,
    });
    state = reduceTurnLatency(state, {
      type: "first_sse",
      turnId: "turn-1",
      atMs: 10_180,
    });
    state = reduceTurnLatency(state, {
      type: "first_visible_token",
      turnId: "turn-1",
      atMs: 10_196,
    });
    state = reduceTurnLatency(state, {
      type: "completed",
      turnId: "turn-1",
      atMs: 12_500,
    });

    expect(summarizeTurnLatency(state)).toMatchObject({
      sendAcknowledgementMs: 12,
      timeToFirstSseMs: 180,
      timeToFirstVisibleTokenMs: 196,
      totalMs: 2_500,
    });
  });

  it("里程碑 first-write-wins，重复动作幂等且旧回合动作被隔离", () => {
    const initial = createTurnLatencyState("turn-current", 1_000);
    const observed = reduceTurnLatency(initial, {
      type: "first_sse",
      turnId: "turn-current",
      atMs: 1_200,
    });
    const duplicate = reduceTurnLatency(observed, {
      type: "first_sse",
      turnId: "turn-current",
      atMs: 1_500,
    });
    const stale = reduceTurnLatency(duplicate, {
      type: "completed",
      turnId: "turn-previous",
      atMs: 2_000,
    });

    expect(duplicate).toBe(observed);
    expect(stale).toBe(observed);
    expect(stale.firstSseAtMs).toBe(1_200);
    expect(stale.completedAtMs).toBeNull();
  });

  it("只把时间夹到回合起点，并保留跨通道乱序携带的真实时间", () => {
    let state = createTurnLatencyState("turn-1", 1_000);
    // Provider 事件先投递到 reducer，但 Host ack 携带的来源时间更早。
    state = reduceTurnLatency(state, {
      type: "first_sse",
      turnId: "turn-1",
      atMs: 1_200,
    });
    state = reduceTurnLatency(state, {
      type: "send_acknowledged",
      turnId: "turn-1",
      atMs: 1_050,
    });
    state = reduceTurnLatency(state, {
      type: "first_visible_token",
      turnId: "turn-1",
      atMs: 900,
    });

    expect([
      state.sendAcknowledgedAtMs,
      state.firstSseAtMs,
      state.firstVisibleTokenAtMs,
    ]).toEqual([1_050, 1_200, 1_000]);
  });

  it("同请求用量按 observationId 去重并只向上更新", () => {
    let state = createTurnLatencyState("turn-1", 1_000);
    state = reduceTurnLatency(state, {
      type: "usage_observed",
      turnId: "turn-1",
      observationId: "request-1",
      inputTokens: 100,
      cacheReadTokens: 40,
      cacheCreationTokens: 10,
    });
    state = reduceTurnLatency(state, {
      type: "usage_observed",
      turnId: "turn-1",
      observationId: "request-1",
      inputTokens: 90,
      cacheReadTokens: 30,
    });
    state = reduceTurnLatency(state, {
      type: "usage_observed",
      turnId: "turn-1",
      observationId: "request-2",
      inputTokens: 300,
      cacheReadTokens: 180,
      cacheCreationTokens: 0,
    });

    expect(state.usageObservations).toHaveLength(2);
    expect(summarizeTurnLatency(state)).toMatchObject({
      inputTokens: 400,
      cacheReadTokens: 220,
      cacheCreationTokens: 10,
      cacheHitRate: 0.55,
    });
  });

  it("区分 Provider 未报告缓存数据与明确的零命中", () => {
    let unknown = createTurnLatencyState("turn-unknown", 1_000);
    unknown = reduceTurnLatency(unknown, {
      type: "usage_observed",
      turnId: "turn-unknown",
      observationId: "request-1",
      inputTokens: 100,
    });
    expect(summarizeTurnLatency(unknown)).toMatchObject({
      cacheReadTokens: null,
      cacheHitRate: null,
    });

    let miss = createTurnLatencyState("turn-miss", 1_000);
    miss = reduceTurnLatency(miss, {
      type: "usage_observed",
      turnId: "turn-miss",
      observationId: "request-1",
      inputTokens: 100,
      cacheReadTokens: 0,
    });
    expect(summarizeTurnLatency(miss)).toMatchObject({
      cacheReadTokens: 0,
      cacheHitRate: 0,
    });
  });

  it("任一请求未报告缓存读取量时不伪造整轮命中率", () => {
    let state = createTurnLatencyState("turn-1", 1_000);
    state = reduceTurnLatency(state, {
      type: "usage_observed",
      turnId: "turn-1",
      observationId: "request-1",
      inputTokens: 100,
      cacheReadTokens: 40,
    });
    state = reduceTurnLatency(state, {
      type: "usage_observed",
      turnId: "turn-1",
      observationId: "request-2",
      inputTokens: 200,
    });

    expect(summarizeTurnLatency(state)).toMatchObject({
      inputTokens: 300,
      cacheReadTokens: null,
      cacheHitRate: null,
    });
  });

  it("直接复用 ACP usage_update 元数据并以 llmStep 去重", () => {
    const action = turnUsageActionFromAcp("turn-1", {
      sessionUpdate: "usage_update",
      used: 300,
      size: 10_000,
      _meta: {
        requestId: "provider-request-1",
        llmStep: 2,
        inputTokens: 250,
        outputTokens: 50,
        cacheReadTokens: 200,
        cacheCreationTokens: 20,
      },
    });

    expect(action).toEqual({
      type: "usage_observed",
      turnId: "turn-1",
      observationId: "turn-1:step:2",
      inputTokens: 250,
      cacheReadTokens: 200,
      cacheCreationTokens: 20,
    });
  });

  it("忽略历史重放或缺少 inputTokens 的 usage_update", () => {
    expect(
      turnUsageActionFromAcp("turn-1", {
        sessionUpdate: "usage_update",
        used: 10,
        size: 100,
        _meta: { periReplay: true, inputTokens: 10, llmStep: 0 },
      }),
    ).toBeNull();
    expect(
      turnUsageActionFromAcp("turn-1", {
        sessionUpdate: "usage_update",
        used: 10,
        size: 100,
      }),
    ).toBeNull();
    expect(
      turnUsageActionFromAcp("turn-1", {
        sessionUpdate: "usage_update",
        used: 10,
        size: 100,
        _meta: { inputTokens: 10 },
      }),
    ).toBeNull();
  });

  it("拒绝无法解释的缓存读取量大于总输入量", () => {
    let state = createTurnLatencyState("turn-1", 1_000);
    state = reduceTurnLatency(state, {
      type: "usage_observed",
      turnId: "turn-1",
      observationId: "request-1",
      inputTokens: 10,
      cacheReadTokens: 11,
    });

    expect(summarizeTurnLatency(state).cacheHitRate).toBeNull();
  });
});

import { describe, expect, it } from "vitest";
import {
  createTurnLatencyState,
  reduceTurnLatency,
  summarizeTurnLatency,
} from "./turnLatency";

describe("turn latency reducer", () => {
  it("投递中断只阻止未知首次观测，保留既有时刻并允许真实完成", () => {
    const acknowledged = reduceTurnLatency(createTurnLatencyState("turn-1", 1000), {
      type: "send_acknowledged", turnId: "turn-1", atMs: 1010,
    });
    const interrupted = reduceTurnLatency(acknowledged, {
      type: "delivery_interrupted", turnId: "turn-1",
    });
    expect(interrupted.deliveryInterrupted).toBe(true);
    expect(reduceTurnLatency(interrupted, { type: "delivery_interrupted", turnId: "turn-1" })).toBe(interrupted);
    expect(reduceTurnLatency(interrupted, { type: "delivery_interrupted", turnId: "other" })).toBe(interrupted);
    for (const type of ["send_acknowledged", "first_sse", "first_token", "first_visible_token"] as const) {
      expect(reduceTurnLatency(interrupted, { type, turnId: "turn-1", atMs: 2000 })).toBe(interrupted);
    }
    const completed = reduceTurnLatency(interrupted, { type: "completed", turnId: "turn-1", atMs: 2100 });
    expect(summarizeTurnLatency(completed)).toMatchObject({
      sendAcknowledgementMs: 10, timeToFirstTokenMs: null, timeToFirstSseMs: null,
      timeToFirstVisibleTokenMs: null, totalMs: 1100,
    });
    const next = createTurnLatencyState("turn-next", 2200);
    expect(next.deliveryInterrupted).toBe(false);
    expect(reduceTurnLatency(next, { type: "first_token", turnId: "turn-next", atMs: 2300 }).firstTokenAtMs).toBe(2300);
  });

  it("中断后迟到的发送ACK不伪造原始接收时刻", () => {
    const interrupted = reduceTurnLatency(createTurnLatencyState("turn-1", 1000), {
      type: "delivery_interrupted", turnId: "turn-1",
    });
    expect(reduceTurnLatency(interrupted, {
      type: "send_acknowledged", turnId: "turn-1", atMs: 5000,
    }).sendAcknowledgedAtMs).toBeNull();
  });

  it("用 null 区分尚未观测的里程碑和用量", () => {
    const state = createTurnLatencyState("turn-1", 1_000);

    expect(state).toMatchObject({
      startedAtMs: 1_000,
      sendAcknowledgedAtMs: null,
      firstSseAtMs: null,
      firstTokenAtMs: null,
      firstVisibleTokenAtMs: null,
      completedAtMs: null,
      usageObservations: [],
    });
    expect(summarizeTurnLatency(state)).toEqual({
      turnId: "turn-1",
      sendAcknowledgementMs: null,
      timeToFirstSseMs: null,
      timeToFirstTokenMs: null,
      timeToFirstVisibleTokenMs: null,
      totalMs: null,
      inputTokens: null,
      reasoningTokens: null,
      cacheReadTokens: null,
      cacheCreationTokens: null,
    });
  });

  it("记录发送确认、首 SSE、首 Token、首个可见 Token 和完成耗时", () => {
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
      type: "first_token",
      turnId: "turn-1",
      atMs: 10_190,
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
      timeToFirstTokenMs: 190,
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

  it("首 Token 首次获胜，迟到 DOM 诊断不改写首 Token 或总耗时", () => {
    let state = createTurnLatencyState("turn-1", 1_000);
    state = reduceTurnLatency(state, {
      type: "first_token",
      turnId: "turn-1",
      atMs: 1_200,
    });
    state = reduceTurnLatency(state, {
      type: "first_token",
      turnId: "turn-1",
      atMs: 1_300,
    });
    state = reduceTurnLatency(state, {
      type: "completed",
      turnId: "turn-1",
      atMs: 1_500,
    });
    state = reduceTurnLatency(state, {
      type: "first_visible_token",
      turnId: "turn-1",
      atMs: 1_700,
    });

    expect(summarizeTurnLatency(state)).toMatchObject({
      timeToFirstTokenMs: 200,
      timeToFirstVisibleTokenMs: 700,
      totalMs: 500,
    });
  });

  it("没有收到非空正文或思考文本时首 Token 保持 null", () => {
    let state = createTurnLatencyState("turn-empty", 1_000);
    state = reduceTurnLatency(state, {
      type: "completed",
      turnId: "turn-empty",
      atMs: 1_500,
    });

    expect(summarizeTurnLatency(state)).toMatchObject({
      timeToFirstTokenMs: null,
    });
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
      reasoningTokens: 25,
      cacheReadTokens: 40,
      cacheCreationTokens: 10,
    });
    state = reduceTurnLatency(state, {
      type: "usage_observed",
      turnId: "turn-1",
      observationId: "request-1",
      inputTokens: 90,
      reasoningTokens: 20,
      cacheReadTokens: 30,
    });
    state = reduceTurnLatency(state, {
      type: "usage_observed",
      turnId: "turn-1",
      observationId: "request-2",
      inputTokens: 300,
      reasoningTokens: 75,
      cacheReadTokens: 180,
      cacheCreationTokens: 0,
    });

    expect(state.usageObservations).toHaveLength(2);
    expect(summarizeTurnLatency(state)).toMatchObject({
      inputTokens: 400,
      reasoningTokens: 100,
      cacheReadTokens: 220,
      cacheCreationTokens: 10,
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
    });
  });

  it("任一请求未报告缓存读取量时保留未知状态", () => {
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
    });
  });

  it("保留供应商上报的原始缓存 Token 供任务聚合层校验", () => {
    let state = createTurnLatencyState("turn-1", 1_000);
    state = reduceTurnLatency(state, {
      type: "usage_observed",
      turnId: "turn-1",
      observationId: "request-1",
      inputTokens: 10,
      cacheReadTokens: 11,
    });

    expect(summarizeTurnLatency(state)).toMatchObject({
      inputTokens: 10,
      cacheReadTokens: 11,
    });
  });
});

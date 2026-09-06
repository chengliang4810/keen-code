import { describe, expect, it } from "vitest";
import type { KeenCodeEventEnvelope, SessionUpdateDeliveryEnvelope } from "@/lib/acp/events";
import { beginSessionRecovery, emptySession, reduceDeliveryEnvelope, type AcpDeliveryReduction } from "@/lib/acp/store";
import { createTurnLatencyState, reduceTurnLatency, summarizeTurnLatency } from "@/lib/turnLatency";
import { observeTurnLatencyDelivery, shouldRecoverFromRuntimeReplay } from "./events";

/** 构造一个不绑定 Turn 的 Runtime 恢复信号。 */
function recoveryEnvelope(
  state: "pending" | "replaying" | "ready" | "failed",
): KeenCodeEventEnvelope {
  return {
    schemaVersion: 1,
    sessionId: "session-1",
    deliverySequence: 1,
    occurredAtMs: 1_000,
    event: { type: "recovery_state_changed", state },
  };
}

describe("Runtime replay recovery trigger", () => {
  it("仅在 replaying 信号已经成功归约后触发恢复", () => {
    const applied: AcpDeliveryReduction = {
      status: "applied",
      ignoredTerminalUpdate: false,
    };
    expect(shouldRecoverFromRuntimeReplay(
      recoveryEnvelope("replaying"),
      applied,
    )).toBe(true);
    expect(shouldRecoverFromRuntimeReplay(
      recoveryEnvelope("ready"),
      applied,
    )).toBe(false);
  });

  it("拒绝重复、冻结和缺口信号启动并发恢复", () => {
    const envelope = recoveryEnvelope("replaying");
    const rejected: AcpDeliveryReduction[] = [
      { status: "duplicate" },
      { status: "stale_generation" },
      { status: "frozen" },
      { status: "gap", expectedSequence: 1, receivedSequence: 2 },
    ];
    expect(rejected.every((result) =>
      !shouldRecoverFromRuntimeReplay(envelope, result)
    )).toBe(true);
  });
});

/** 已经由共享水位接受的根事件。 */
const appliedRoot: AcpDeliveryReduction = {status: "applied", ignoredTerminalUpdate: false};

/** 构造来源墙钟可独立跳动的根生命周期信号。 */
function latencyEvent(event: KeenCodeEventEnvelope["event"], occurredAtMs = 999_999): KeenCodeEventEnvelope {
  return {schemaVersion: 1, sessionId: "session-1", turnId: "turn-1", sourceAgentId: "root", deliverySequence: 1, occurredAtMs, event};
}

/** 构造真实标准文本块；采样不依赖该文本是否已挂载DOM。 */
function latencyText(text: string, thought = false): SessionUpdateDeliveryEnvelope {
  return {schemaVersion: 1, sessionId: "session-1", turnId: "turn-1", sourceAgentId: "root", deliverySequence: 2, occurredAtMs: 1,
    update: {sessionUpdate: thought ? "agent_thought_chunk" : "agent_message_chunk", content: {type: "text", text}}};
}

describe("根Turn端到端接收耗时", () => {
  it("发送确认、首流、首文本及完成只用同一前端单调时钟", () => {
    let state = createTurnLatencyState("turn-1", 1000);
    state = observeTurnLatencyDelivery(state, latencyEvent({type: "turn_started", rootTurnId: "turn-1"}), appliedRoot, false, 1010);
    state = observeTurnLatencyDelivery(state, latencyEvent({type: "model_first_stream_observed"}), appliedRoot, false, 1020);
    state = observeTurnLatencyDelivery(state, latencyText("正文"), appliedRoot, false, 1030);
    state = observeTurnLatencyDelivery(state, latencyEvent({type: "turn_completed"}, 0), appliedRoot, false, 1040);
    expect(summarizeTurnLatency(state)).toMatchObject({sendAcknowledgementMs: 10, timeToFirstSseMs: 20, timeToFirstTokenMs: 30, totalMs: 40, timeToFirstVisibleTokenMs: null});
  });

  it("后台完成后才提交DOM不会把用户返回页面的等待计入首Token", () => {
    let state = createTurnLatencyState("turn-1", 1000);
    state = observeTurnLatencyDelivery(state, latencyText("后台正文"), appliedRoot, false, 1200);
    state = observeTurnLatencyDelivery(state, latencyEvent({type: "turn_completed"}), appliedRoot, false, 1250);
    const visible = reduceTurnLatency(state, {type: "first_visible_token", turnId: "turn-1", atMs: 90_000});
    expect(summarizeTurnLatency(visible)).toMatchObject({timeToFirstTokenMs: 200, totalMs: 250, timeToFirstVisibleTokenMs: 89_000});
    expect(observeTurnLatencyDelivery(visible, latencyText("迟到内容"), appliedRoot, false, 91_000)).toBe(visible);
  });

  it("先到的思考是首Token，后续正文和重复chunk不改写首时刻", () => {
    const initial = createTurnLatencyState("turn-1", 1000);
    const thought = observeTurnLatencyDelivery(initial, latencyText("思考", true), appliedRoot, false, 1015);
    expect(thought.firstTokenAtMs).toBe(1015);
    expect(observeTurnLatencyDelivery(thought, latencyText("正文"), appliedRoot, false, 1050)).toBe(thought);
  });

  it("首流和空文本不冒充Token；合法空白文本仍属于模型输出", () => {
    const initial = createTurnLatencyState("turn-1", 1000);
    const stream = observeTurnLatencyDelivery(initial, latencyEvent({type: "model_first_stream_observed"}), appliedRoot, false, 1010);
    expect(stream.firstTokenAtMs).toBeNull();
    expect(observeTurnLatencyDelivery(stream, latencyText(""), appliedRoot, false, 1020)).toBe(stream);
    expect(observeTurnLatencyDelivery(stream, latencyText(" "), appliedRoot, false, 1030).firstTokenAtMs).toBe(1030);
    const image = {...latencyText(""), update: {sessionUpdate: "agent_message_chunk" as const, content: {type: "image" as const, data: "AA==", mimeType: "image/png"}}};
    expect(observeTurnLatencyDelivery(stream, image, appliedRoot, false, 1040)).toBe(stream);
  });

  it("拒绝子Agent、回放、重复、缺口、忽略终态及其他Turn污染指标", () => {
    const state = createTurnLatencyState("turn-1", 1000);
    const ignored: AcpDeliveryReduction[] = [
      {status: "duplicate"}, {status: "frozen"}, {status: "stale_generation"},
      {status: "gap", expectedSequence: 1, receivedSequence: 3},
      {status: "applied", ignoredTerminalUpdate: true},
      {status: "applied", ignoredTerminalUpdate: false, childAgentId: "child-1"},
    ];
    ignored.forEach(reduction => expect(observeTurnLatencyDelivery(state, latencyText("正文"), reduction, false, 1100)).toBe(state));
    expect(observeTurnLatencyDelivery(state, latencyText("正文"), appliedRoot, true, 1100)).toBe(state);
    expect(observeTurnLatencyDelivery(state, {...latencyText("正文"), turnId: "other"}, appliedRoot, false, 1100)).toBe(state);
  });

  it.each(["turn_completed", "turn_cancelled"] as const)("无正文的%s保持首Token未知", type => {
    const state = observeTurnLatencyDelivery(createTurnLatencyState("turn-1", 1000), latencyEvent({type}), appliedRoot, false, 1200);
    expect(summarizeTurnLatency(state)).toMatchObject({timeToFirstTokenMs: null, totalMs: 200});
  });

  it("真实共享归约器的根终态与迟到正文屏障保持首次接收观测", () => {
    const view = emptySession("session-1");
    let state = createTurnLatencyState("turn-1", 1000);
    const deliveries = [
      latencyEvent({type: "turn_started", rootTurnId: "turn-1"}),
      latencyText("后台完成正文"),
      {...latencyEvent({type: "turn_completed"}), deliverySequence: 3},
    ];
    deliveries.forEach((envelope, index) => {
      const restoring = view.replay.restoring;
      const reduction = reduceDeliveryEnvelope(view, envelope);
      state = observeTurnLatencyDelivery(state, envelope, reduction, restoring, 1010 + index * 10);
    });
    expect(summarizeTurnLatency(state)).toMatchObject({sendAcknowledgementMs: 10, timeToFirstTokenMs: 20, totalMs: 30});
    expect(view.history.at(-1)?.content).toBe("后台完成正文");
    const late = {...latencyText("迟到"), deliverySequence: 4};
    const reduction = reduceDeliveryEnvelope(view, late);
    expect(reduction).toEqual({status: "applied", ignoredTerminalUpdate: true});
    expect(observeTurnLatencyDelivery(state, late, reduction, false, 9000)).toBe(state);
  });

  it("真实子Agent归属、重复及缺口冻结不会提前记录根首Token", () => {
    const view = emptySession("session-1");
    const state = createTurnLatencyState("turn-1", 1000);
    const spawned = latencyEvent({type: "agent_spawned", agentId: "child-1", parentAgentId: "root", agentPath: "/root/review", task: "核对", parentTurnId: "turn-1", rootTurnId: "turn-1"});
    reduceDeliveryEnvelope(view, spawned);
    const child = {...latencyText("子正文"), sourceAgentId: "child-1", turnId: "child-turn-1"};
    const reduction = reduceDeliveryEnvelope(view, child);
    expect(reduction).toMatchObject({status: "applied", childAgentId: "child-1"});
    expect(observeTurnLatencyDelivery(state, child, reduction, false, 1010)).toBe(state);
    const duplicate = latencyText("重复水位根正文");
    const duplicateResult = reduceDeliveryEnvelope(view, duplicate);
    expect(duplicateResult.status).toBe("duplicate");
    expect(observeTurnLatencyDelivery(state, duplicate, duplicateResult, false, 1020)).toBe(state);
    const gap = {...latencyText("缺口根正文"), deliverySequence: 4};
    const gapResult = reduceDeliveryEnvelope(view, gap);
    expect(gapResult.status).toBe("gap");
    expect(observeTurnLatencyDelivery(state, gap, gapResult, false, 1030)).toBe(state);
    beginSessionRecovery(view);
    const replay = {...latencyText("回放正文"), deliverySequence: 1};
    const restoring = view.replay.restoring;
    const replayResult = reduceDeliveryEnvelope(view, replay);
    expect(restoring).toBe(true);
    expect(replayResult.status).toBe("applied");
    expect(observeTurnLatencyDelivery(state, replay, replayResult, restoring, 1040)).toBe(state);
  });
});

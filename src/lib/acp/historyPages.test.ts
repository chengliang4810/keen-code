import { describe, expect, it } from "vitest";
import { parseHistoryPage, prependHistoryPage } from "./historyPages";
import { emptySession } from "./store";

const text = (sessionId: string, deliverySequence: number, value: string) => ({
  type: "session_update",
  envelope: { schemaVersion: 1, sessionId, turnId: "old-turn", sourceAgentId: "root", deliverySequence,
    occurredAtMs: 1000, update: { sessionUpdate: "user_message_chunk", content: { type: "text", text: value } } },
});

describe("独立历史分页投影", () => {
  it("拒绝错误 Session 和缺口，失败不会修改当前投影", () => {
    for (const deliveries of [[text("other", 1, "bad")], [text("s", 2, "gap")], [text("s", 1, "a"), text("s", 1, "duplicate")]]) {
      const view = emptySession("s");
      const before = structuredClone(view);
      expect(() => prependHistoryPage(view, { sessionId: "s", nextCursor: null, hasMore: false, deliveries })).toThrow();
      expect(view).toEqual(before);
    }
  });
  it("旧历史前插，保留当前轮次、水位及配置", () => {
    const view = emptySession("s");
    view.active_root_turn_id = "live";
    view.delivery.lastSequence = 99;
    view.title = "current title";
    prependHistoryPage(view, { sessionId: "s", nextCursor: null, hasMore: false, deliveries: [text("s", 1, "old question")] });
    expect(view.history).toHaveLength(1);
    expect(view.history[0]?.content).toBe("old question");
    expect(view.active_root_turn_id).toBe("live");
    expect(view.delivery.lastSequence).toBe(99);
    expect(view.title).toBe("current title");
  });
  it("拒绝不一致的分页结束标识", () => {
    expect(() => parseHistoryPage({ "keencode/history": { sessionId: "s", hasMore: true, nextCursor: null, deliveries: [] } }, "s")).toThrow();
  });
});

it("跨页子任务合并正文和轮次区间，保持最近终态", () => {
  const lifecycle = (sequence: number, event: object, child = true) => ({
    type: "keencode_event",
    envelope: { schemaVersion: 1, sessionId: "s", deliverySequence: sequence, occurredAtMs: 1000 + sequence,
      turnId: child ? "child-turn" : "root-turn", sourceAgentId: child ? "child" : "root",
      journalSequence: sequence, event },
  });
  const spawn = lifecycle(1, { type: "agent_spawned", agentId: "child", parentAgentId: "root",
    agentPath: "root/child", task: "task", parentTurnId: "root-turn", rootTurnId: "root-turn" }, false);
  const start = lifecycle(2, { type: "turn_started", rootTurnId: "root-turn", parentTurnId: "root-turn" });
  const chunk = (value: string) => ({ type: "session_update", envelope: {
    ...text("s", 3, value).envelope, turnId: "child-turn", sourceAgentId: "child",
    update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: value } },
  } });
  const view = emptySession("s");
  prependHistoryPage(view, { sessionId: "s", hasMore: true, nextCursor: "older",
    deliveries: [spawn, start, chunk("second"), lifecycle(4, { type: "turn_completed" })] });
  prependHistoryPage(view, { sessionId: "s", hasMore: false, nextCursor: null,
    deliveries: [spawn, start, chunk("first")] });
  const agent = view.subagents[0]!;
  expect(agent.status).toBe("done");
  expect(agent.segments).toHaveLength(2);
  expect(agent.turns).toHaveLength(1);
  expect(agent.turns![0]?.segmentStart).toBe(0);
  expect(agent.turns![0]?.segmentEnd).toBe(2);
});

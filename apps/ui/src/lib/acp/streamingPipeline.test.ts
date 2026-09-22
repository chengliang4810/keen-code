import { describe, expect, it } from "vitest";
import { parseAcpTauriDelivery } from "./events";
import { emptySession, reduceDeliveryEnvelope } from "./store";
import { projectAcpConversation } from "../sessionProjection";

describe("ACP streaming frontend pipeline", () => {
  it("JSON contract -> reducer -> projection keeps lifecycle and text order", () => {
    const view = emptySession("session-e2e");
    const rawDeliveries = [
      {
        type: "keencode_event",
        envelope: {
          schemaVersion: 1,
          sessionId: "session-e2e",
          turnId: "turn-e2e",
          sourceAgentId: "root",
          journalSequence: 1,
          deliverySequence: 1,
          occurredAtMs: 1,
          event: { type: "turn_started", rootTurnId: "turn-e2e" },
        },
      },
      {
        type: "session_update",
        envelope: {
          schemaVersion: 1,
          sessionId: "session-e2e",
          turnId: "turn-e2e",
          sourceAgentId: "root",
          deliverySequence: 2,
          occurredAtMs: 2,
          update: {
            sessionUpdate: "agent_message_chunk",
            content: { type: "text", text: "第一段" },
          },
        },
      },
      {
        type: "session_update",
        envelope: {
          schemaVersion: 1,
          sessionId: "session-e2e",
          turnId: "turn-e2e",
          sourceAgentId: "root",
          deliverySequence: 3,
          occurredAtMs: 3,
          update: {
            sessionUpdate: "agent_message_chunk",
            content: { type: "text", text: "第二段" },
          },
        },
      },
      {
        type: "keencode_event",
        envelope: {
          schemaVersion: 1,
          sessionId: "session-e2e",
          turnId: "turn-e2e",
          sourceAgentId: "root",
          journalSequence: 2,
          deliverySequence: 4,
          occurredAtMs: 4,
          event: { type: "turn_completed" },
        },
      },
    ];

    for (const raw of rawDeliveries) {
      const delivery = parseAcpTauriDelivery(raw);
      expect(delivery).not.toBeNull();
      if (!delivery || !("envelope" in delivery)) throw new Error("测试投递解析失败");
      const result = reduceDeliveryEnvelope(view, delivery.envelope);
      expect(result.status).toBe("applied");
    }

    const messages = projectAcpConversation([], view, "zh");
    expect(messages.at(-1)).toMatchObject({
      role: "assistant",
      content: "第一段第二段",
      streaming: false,
      turnStatus: "completed",
    });
    expect(view.delivery.lastSequence).toBe(4);
    expect(view.active_root_turn_id).toBeNull();
    expect(view.history.at(-1)?.content).toBe("第一段第二段");
  });
});

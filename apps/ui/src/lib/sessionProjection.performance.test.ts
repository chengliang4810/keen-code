import { describe, expect, it } from "vitest";
import type { KeenCodeEvent, SessionUpdate } from "./acp/events";
import {
  emptySession,
  reduceDeliveryEnvelope,
  type AcpSessionView,
} from "./acp/store";
import {
  projectAcpConversation,
  projectAcpLiveMessage,
} from "./sessionProjection";
import {
  compactMessageSegments,
  deriveFieldsFromSegments,
  type ChatMessage,
} from "./session";
import { createAnimationFrameBatcher } from "./frameBatcher";
import { computeChatVirtualWindow } from "./chatVirtualList";

function applyUpdate(
  view: AcpSessionView,
  sequence: number,
  update: SessionUpdate,
): void {
  const result = reduceDeliveryEnvelope(view, {
    schemaVersion: 1,
    sessionId: view.session_id,
    turnId: "turn-performance",
    sourceAgentId: "root",
    deliverySequence: sequence,
    occurredAtMs: sequence,
    update,
  });
  if (result.status !== "applied") {
    throw new Error(`performance fixture delivery was not applied: ${result.status}`);
  }
}

function applyEvent(
  view: AcpSessionView,
  sequence: number,
  event: KeenCodeEvent,
): void {
  const result = reduceDeliveryEnvelope(view, {
    schemaVersion: 1,
    sessionId: view.session_id,
    turnId: "turn-performance",
    sourceAgentId: "root",
    deliverySequence: sequence,
    occurredAtMs: sequence,
    event,
  });
  if (result.status !== "applied") {
    throw new Error(`performance fixture event was not applied: ${result.status}`);
  }
}

describe("streaming projection pressure", () => {
  it("records a repeatable 100ms-style projection workload", () => {
    const view = emptySession("performance-session");
    applyEvent(view, 1, { type: "turn_started", rootTurnId: "turn-performance" });

    const chunk = "streaming token ".repeat(4);
    const chunkCount = 6_000;
    const projectionEvery = 10;
    const expected = chunk.repeat(chunkCount);
    let projectedContent = "";
    let projectionCount = 0;
    const started = performance.now();

    for (let index = 0; index < chunkCount; index += 1) {
      applyUpdate(view, index + 2, {
        sessionUpdate: "agent_message_chunk",
        content: { type: "text", text: chunk },
      });
      if ((index + 1) % projectionEvery === 0) {
        const projected = projectAcpConversation([], view, "zh", true);
        projectedContent = projected.at(-1)?.content ?? "";
        projectionCount += 1;
      }
    }

    const elapsedMs = performance.now() - started;
    expect(projectedContent).toBe(expected);
    expect(projectionCount).toBe(chunkCount / projectionEvery);
    // Keep the pressure case bounded without making CI depend on wall-clock
    // speed; the log is the comparable before/after evidence for this workload.
    expect(elapsedMs).toBeGreaterThanOrEqual(0);
    console.log(
      `[streaming-projection-pressure] chunks=${chunkCount} projections=${projectionCount} ` +
        `sourceChars=${expected.length} elapsedMs=${elapsedMs.toFixed(1)}`,
    );
  });

  it("compares cached live fields with the previous full-field reduction", () => {
    const chunk = "streaming token ".repeat(4);
    const chunkCount = 6_000;
    const projectionEvery = 10;

    const run = (
      project: (view: AcpSessionView) => ChatMessage | null,
    ): { elapsedMs: number; content: string } => {
      const view = emptySession("live-field-pressure");
      applyEvent(view, 1, {
        type: "turn_started",
        rootTurnId: "turn-performance",
      });
      const started = performance.now();
      let content = "";
      for (let index = 0; index < chunkCount; index += 1) {
        applyUpdate(view, index + 2, {
          sessionUpdate: "agent_message_chunk",
          content: { type: "text", text: chunk },
        });
        if ((index + 1) % projectionEvery === 0) {
          content = project(view)?.content ?? "";
        }
      }
      return { elapsedMs: performance.now() - started, content };
    };

    const uncached = run((view) => {
      const segments = compactMessageSegments(view.live_segments);
      const fields = deriveFieldsFromSegments(segments);
      return {
        id: "baseline",
        role: "assistant",
        content: fields.content,
      };
    });
    const cached = run(projectAcpLiveMessage);

    expect(cached.content).toBe(chunk.repeat(chunkCount));
    expect(cached.elapsedMs).toBeGreaterThanOrEqual(0);
    console.log(
      `[live-field-pressure] uncachedMs=${uncached.elapsedMs.toFixed(1)} ` +
        `cachedMs=${cached.elapsedMs.toFixed(1)} ` +
        `reduction=${((1 - cached.elapsedMs / Math.max(uncached.elapsedMs, 0.001)) * 100).toFixed(1)}%`,
    );
  });

  it("batches high-frequency deltas across sessions while reusing history and windowing the transcript", () => {
    const sessions = Array.from({ length: 3 }, (_, sessionIndex) => {
      const view = emptySession(`pressure-session-${sessionIndex}`);
      view.history = Array.from({ length: 120 }, (_, messageIndex) => ({
        role: messageIndex % 2 === 0 ? "user" as const : "assistant" as const,
        content: `history-${sessionIndex}-${messageIndex}`,
      }));
      applyEvent(view, 1, { type: "turn_started", rootTurnId: "turn-performance" });
      return view;
    });
    const callbacks: FrameRequestCallback[] = [];
    let requestedFrames = 0;
    let publishCount = 0;
    const firstHistoryProjection = sessions.map((view) =>
      projectAcpConversation([], view, "zh", true)[0],
    );
    let latest = sessions.map(() => [] as ChatMessage[]);
    const batcher = createAnimationFrameBatcher(
      () => {
        latest = sessions.map((view) => projectAcpConversation([], view, "zh", true));
        publishCount += 1;
      },
      (callback) => {
        requestedFrames += 1;
        callbacks.push(callback);
        return requestedFrames;
      },
      () => {},
    );

    const chunkCount = 2_000;
    for (const view of sessions) {
      for (let index = 0; index < chunkCount; index += 1) {
        applyUpdate(view, index + 2, {
          sessionUpdate: "agent_message_chunk",
          content: { type: "text", text: `${view.session_id}:${index};` },
        });
        batcher.schedule();
        if ((index + 1) % 100 === 0) callbacks.shift()?.(index);
      }
    }

    expect(requestedFrames).toBe(60);
    expect(publishCount).toBe(60);
    sessions.forEach((view, sessionIndex) => {
      const projected = latest[sessionIndex]!;
      expect(projected).toHaveLength(121);
      expect(projected[0]).toBe(firstHistoryProjection[sessionIndex]);
      expect(projected.at(-1)?.content).toContain(`${view.session_id}:1999;`);
      const windowed = computeChatVirtualWindow({
        count: projected.length,
        getHeight: () => 120,
        scrollTop: 0,
        viewportHeight: 720,
        overscanPx: 600,
        pinToBottom: true,
      });
      expect(windowed.end).toBe(projected.length);
      expect(windowed.start).toBeGreaterThan(0);
      expect(windowed.end - windowed.start).toBeLessThan(projected.length);
      expect(windowed.paddingTop).toBeGreaterThan(0);
    });
  });
});

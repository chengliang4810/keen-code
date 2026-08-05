/**
 * End-to-end pure fixture path for chat UX renovation.
 * Drives shipped reducers/helpers only (no UI mocks of the unit under test).
 */
import { describe, expect, it } from "vitest";
import {
  applyStreamChunk,
  applyToolEvent,
  applyTurnMarker,
  buildSegmentsFromFields,
  isFailedToolStepMessage,
  messageSegments,
  type ChatMessage,
} from "./session";
import { extractThinkingSummary } from "./thinkingSummary";
import { buildTurnActivity } from "./turnActivity";
import { buildTimelineUnits } from "./timelinePhases";
import { mapEndOfTurnReason } from "./endOfTurn";
import {
  armStopLatch,
  createStopLatchState,
  tickStopLatch,
  canSendWithStopLatch,
  STOP_LATCH_MS,
} from "./stopLatch";

describe("chat UX fixtures (shipped path)", () => {
  it("a) multi-phase thoughts with empty-assistant-style new hints merge", () => {
    let messages: ChatMessage[] = [
      { id: "u1", role: "user", content: "q" },
      {
        id: "a1",
        role: "assistant",
        content: "",
        segments: [{ kind: "thought", text: "**定位目录**" }],
        streaming: true,
      },
    ];
    // Spurious "new" without body between thoughts
    messages = applyStreamChunk(messages, {
      sessionId: "s",
      messageId: "a1",
      text: "更多推理",
      done: false,
      kind: "thought",
      thoughtPhase: "new",
    });
    messages = applyStreamChunk(messages, {
      sessionId: "s",
      messageId: "a1",
      text: "答案正文",
      done: true,
      kind: "assistant",
    });
    const segs = messageSegments(messages[1]!);
    const thoughtLabels = segs
      .filter((s) => s.kind === "thought")
      .map((s) => extractThinkingSummary(s.text) || s.text);
    for (const lab of thoughtLabels) {
      expect(lab).not.toMatch(/^思考\s*\d+$/);
      expect(lab).not.toMatch(/^Thinking\s*\d+$/i);
    }
    // Reload path: multi phase markers stack before body only
    const segments = buildSegmentsFromFields(
      "答案正文",
      "a\n\n⟪phase⟫\n\nb\n\n⟪phase⟫\n\nc",
    );
    expect(segments.map((s) => s.kind)).toEqual(["thought", "content"]);
    expect(segments[0]!.kind === "thought" && segments[0]!.text).toContain("a");
  });

  it("b) failed tools counted in activity; tools pin onto assistant timeline", () => {
    let messages: ChatMessage[] = [
      { id: "u1", role: "user", content: "do" },
      {
        id: "a1",
        role: "assistant",
        content: "",
        segments: [{ kind: "thought", text: "plan" }],
        streaming: true,
      },
    ];
    for (let i = 0; i < 5; i++) {
      messages = applyToolEvent(messages, {
        sessionId: "s",
        toolCallId: `ok-${i}`,
        title: `Read f${i}`,
        kind: "Read",
        status: "completed",
        path: `/p/f${i}.ts`,
      });
    }
    messages = applyToolEvent(messages, {
      sessionId: "s",
      toolCallId: "bad",
      title: "Shell boom",
      kind: "Execute",
      status: "failed",
      detail: "exit 1",
    });
    const tools = messages.filter((m) => m.marker === "tool_step");
    const failed = tools.filter(isFailedToolStepMessage);
    const success = tools.filter((m) => !isFailedToolStepMessage(m));
    expect(failed).toHaveLength(1);
    expect(success.length).toBeGreaterThanOrEqual(5);
    // Tasks panel still derives from tool_step rows
    const act = buildTurnActivity(messages);
    expect(act.errorCount).toBe(1);
    expect(act.shouldExpand).toBe(true);
    expect(act.stepCount).toBe(6);
    // Assistant segments include tools on the real timeline
    const asst = messages.find((m) => m.id === "a1")!;
    const segs = messageSegments(asst);
    expect(segs.some((s) => s.kind === "tool")).toBe(true);
    expect(segs.filter((s) => s.kind === "tool")).toHaveLength(6);
    const bad = segs.find(
      (s) => s.kind === "tool" && s.toolCallId === "bad",
    );
    expect(bad && bad.kind === "tool" && bad.isError).toBe(true);
  });

  it("b2) live stream interleaves thought → tool → content", () => {
    let messages: ChatMessage[] = [
      { id: "u1", role: "user", content: "fix" },
    ];
    messages = applyStreamChunk(messages, {
      sessionId: "s",
      messageId: "a1",
      text: "先查一下",
      done: false,
      kind: "thought",
    });
    messages = applyToolEvent(messages, {
      toolCallId: "t1",
      title: "Read foo.ts",
      kind: "Read",
      status: "completed",
      path: "/src/foo.ts",
    });
    messages = applyStreamChunk(messages, {
      sessionId: "s",
      messageId: "a1",
      text: "修好了。",
      done: true,
      kind: "assistant",
    });
    const segs = messageSegments(messages.find((m) => m.role === "assistant")!);
    expect(segs.map((s) => s.kind)).toEqual(["thought", "tool", "content"]);
    expect(segs[1]!.kind === "tool" && segs[1]!.title).toContain("foo");
  });

  it("b3b) phase closes when content arrives mid-stream (not only at turn end)", () => {
    let messages: ChatMessage[] = [
      { id: "u1", role: "user", content: "go" },
    ];
    messages = applyStreamChunk(messages, {
      sessionId: "s",
      messageId: "a1",
      text: "**定位** 问题",
      done: false,
      kind: "thought",
    });
    messages = applyToolEvent(messages, {
      toolCallId: "t1",
      title: "Read a",
      kind: "Read",
      status: "completed",
    });
    messages = applyToolEvent(messages, {
      toolCallId: "t2",
      title: "Grep b",
      kind: "Grep",
      status: "completed",
    });
    // Still streaming — work phase is live
    let segs = messageSegments(messages.find((m) => m.role === "assistant")!);
    let units = buildTimelineUnits(segs, { streaming: true });
    expect(units.map((u) => u.kind)).toEqual(["thought", "phase"]);
    if (units[1]?.kind === "phase") expect(units[1].live).toBe(true);

    // Content starts → phase closes even though stream continues
    messages = applyStreamChunk(messages, {
      sessionId: "s",
      messageId: "a1",
      text: "结论。",
      done: false,
      kind: "assistant",
    });
    segs = messageSegments(messages.find((m) => m.role === "assistant")!);
    units = buildTimelineUnits(segs, { streaming: true });
    expect(units.map((u) => u.kind)).toEqual([
      "thought",
      "phase",
      "content",
    ]);
    if (units[1]?.kind === "phase") expect(units[1].live).toBe(false);
  });

  it("b3) tools before first stream token prepend onto assistant", () => {
    let messages: ChatMessage[] = [
      { id: "u1", role: "user", content: "go" },
    ];
    messages = applyToolEvent(messages, {
      toolCallId: "early",
      title: "Glob dir",
      kind: "Glob",
      status: "completed",
    });
    messages = applyStreamChunk(messages, {
      sessionId: "s",
      messageId: "a1",
      text: "看完了",
      done: true,
      kind: "assistant",
    });
    const segs = messageSegments(messages.find((m) => m.role === "assistant")!);
    expect(segs.map((s) => s.kind)).toEqual(["tool", "content"]);
    expect(segs[0]!.kind === "tool" && segs[0]!.toolCallId).toBe("early");
  });

  it("c) multi-tool turn activity groups context tools", () => {
    let messages: ChatMessage[] = [
      { id: "u1", role: "user", content: "explore" },
    ];
    for (const [id, kind] of [
      ["a", "Read"],
      ["b", "Grep"],
      ["c", "Glob"],
    ] as const) {
      messages = applyToolEvent(messages, {
        toolCallId: id,
        title: `${kind} ${id}`,
        kind,
        status: "completed",
        path: `/src/${id}.ts`,
      });
    }
    messages = applyToolEvent(messages, {
      toolCallId: "e",
      title: "Edit",
      kind: "Edit",
      status: "completed",
      path: "/src/a.ts",
    });
    const act = buildTurnActivity(messages);
    expect(act.stepCount).toBe(4);
    expect(act.segments.some((s) => s.kind === "context")).toBe(true);
    if (act.segments[0]?.kind === "context") {
      expect(act.segments[0].tools.length).toBeGreaterThanOrEqual(3);
    }
    expect(act.modifiedPaths.length).toBeGreaterThanOrEqual(1);
  });

  it("d) end reasons map to one chip family; stop latch unlocks send", () => {
    expect(mapEndOfTurnReason("user_stop").messageKey).toBe(
      "activity.cancelledByUser",
    );
    expect(mapEndOfTurnReason("stall").messageKey).toBe("endOfTurn.stall");
    let messages: ChatMessage[] = [
      { id: "u1", role: "user", content: "x" },
      { id: "a1", role: "assistant", content: "partial", streaming: true },
    ];
    messages = applyTurnMarker(messages, {
      marker: "turn_end",
      reason: "user_stop",
      content: "turn_end|user_stop",
    });
    expect(messages.some((m) => m.marker === "turn_end")).toBe(true);

    let latch = armStopLatch(createStopLatchState(), "s1", 0);
    expect(canSendWithStopLatch("streaming", latch)).toBe(false);
    const r = tickStopLatch(latch, "streaming", STOP_LATCH_MS);
    expect(r.forceComplete).toBe(true);
    expect(canSendWithStopLatch("streaming", r.latch)).toBe(true);
  });
});

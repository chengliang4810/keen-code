/**
 * End-to-end pure fixture path for chat UX renovation.
 *
 * Fixtures drive the ACP reducer and its message projection. The UI helpers
 * only receive the projected ChatMessage[]; legacy stream/tool reducers are not
 * part of this path.
 */
import { describe, expect, it } from "vitest";
import {
  applyTurnMarker,
  buildSegmentsFromFields,
  messageSegments,
  type ChatMessage,
} from "./session";
import {
  emptySession,
  reduceSessionUpdate,
  type AcpSessionView,
} from "./acp/store";
import { projectAcpConversation } from "./sessionProjection";
import { buildTimelineUnits } from "./timelinePhases";
import { mapEndOfTurnReason } from "./endOfTurn";
import {
  armStopLatch,
  createStopLatchState,
  tickStopLatch,
  canSendWithStopLatch,
  STOP_LATCH_MS,
} from "./stopLatch";
import type { SessionUpdate } from "./acp/events";

function fixtureView(userText: string): AcpSessionView {
  const view = emptySession("s");
  reduceSessionUpdate(view, {
    sessionUpdate: "user_message_chunk",
    content: { type: "text", text: userText },
  });
  view.status = "streaming";
  return view;
}

function reduce(view: AcpSessionView, update: SessionUpdate): void {
  reduceSessionUpdate(view, update);
  view.status = "streaming";
}

function project(view: AcpSessionView): ChatMessage[] {
  return projectAcpConversation([], view, "zh", true);
}

function assistant(view: AcpSessionView): ChatMessage {
  const message = project(view).find((item) => item.role === "assistant");
  if (!message) throw new Error("fixture expected an Assistant projection");
  return message;
}

describe("chat UX fixtures (ACP shipped path)", () => {
  it("a) late thoughts stay Thinking segments and never enter answer content", () => {
    const view = fixtureView("q");
    reduce(view, {
      sessionUpdate: "agent_thought_chunk",
      content: { type: "text", text: "定位目录" },
    });
    reduce(view, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "答案正文" },
    });
    // A late thought remains a thought segment, preserving the original event
    // semantics instead of mutating the already emitted answer text.
    reduce(view, {
      sessionUpdate: "agent_thought_chunk",
      content: { type: "text", text: "补充推理" },
    });

    const projected = assistant(view);
    expect(projected.content).toBe("答案正文");
    expect(projected.segments?.map((segment) => segment.kind)).toEqual([
      "thought",
      "content",
      "thought",
    ]);
    expect(projected.segments?.[2]).toEqual({
      kind: "thought",
      text: "补充推理",
    });

    // Reload path: multi-phase fields stack before body only.
    const segments = buildSegmentsFromFields(
      "答案正文",
      "a\n\n⟪phase⟫\n\nb\n\n⟪phase⟫\n\nc",
    );
    expect(segments.map((segment) => segment.kind)).toEqual([
      "thought",
      "content",
    ]);
    expect(segments[0]!.kind === "thought" && segments[0]!.text).toContain(
      "a",
    );
  });

  it("b) failed tools stay on the ACP assistant timeline", () => {
    const view = fixtureView("do");
    reduce(view, {
      sessionUpdate: "agent_thought_chunk",
      content: { type: "text", text: "plan" },
    });
    for (let i = 0; i < 5; i += 1) {
      reduce(view, {
        sessionUpdate: "tool_call",
        toolCallId: `ok-${i}`,
        title: `Read f${i}`,
        kind: "read",
        status: "completed",
        rawInput: { file_path: `/p/f${i}.ts` },
      });
    }
    reduce(view, {
      sessionUpdate: "tool_call",
      toolCallId: "bad",
      title: "Shell boom",
      kind: "execute",
      status: "failed",
      rawInput: { cmd: "exit 1" },
    });
    reduce(view, {
      sessionUpdate: "tool_call_update",
      toolCallId: "bad",
      status: "failed",
      rawOutput: "exit 1",
    });

    const segments = messageSegments(assistant(view));
    const tools = segments.filter((segment) => segment.kind === "tool");
    expect(tools).toHaveLength(6);
    const failed = tools.find(
      (segment) => segment.kind === "tool" && segment.toolCallId === "bad",
    );
    expect(failed).toMatchObject({
      title: "Shell boom",
      status: "failed",
      output: "exit 1",
      isError: true,
    });
  });

  it("b2) ACP events interleave thought → tool → content", () => {
    const view = fixtureView("fix");
    reduce(view, {
      sessionUpdate: "agent_thought_chunk",
      content: { type: "text", text: "先查一下" },
    });
    reduce(view, {
      sessionUpdate: "tool_call",
      toolCallId: "t1",
      title: "Read foo.ts",
      kind: "read",
      status: "completed",
      rawInput: { file_path: "/src/foo.ts" },
    });
    reduce(view, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "修好了。" },
    });

    const segments = messageSegments(assistant(view));
    expect(segments.map((segment) => segment.kind)).toEqual([
      "thought",
      "tool",
      "content",
    ]);
    expect(segments[1]!.kind === "tool" && segments[1]!.title).toContain(
      "foo",
    );
  });

  it("b3b) content closes a live tool phase before turn end", () => {
    const view = fixtureView("go");
    reduce(view, {
      sessionUpdate: "agent_thought_chunk",
      content: { type: "text", text: "定位问题" },
    });
    for (const [toolCallId, title, kind] of [
      ["t1", "Read a", "read"],
      ["t2", "Grep b", "search"],
    ] as const) {
      reduce(view, {
        sessionUpdate: "tool_call",
        toolCallId,
        title,
        kind,
        status: "in_progress",
      });
    }

    let segments = messageSegments(assistant(view));
    let units = buildTimelineUnits(segments, { streaming: true });
    expect(units.map((unit) => unit.kind)).toEqual(["thought", "phase"]);
    if (units[1]?.kind === "phase") expect(units[1].live).toBe(true);

    reduce(view, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "结论。" },
    });
    segments = messageSegments(assistant(view));
    units = buildTimelineUnits(segments, { streaming: true });
    expect(units.map((unit) => unit.kind)).toEqual([
      "thought",
      "phase",
      "content",
    ]);
    if (units[1]?.kind === "phase") expect(units[1].live).toBe(false);
  });

  it("b3) tools before the first text remain in ACP arrival order", () => {
    const view = fixtureView("go");
    reduce(view, {
      sessionUpdate: "tool_call",
      toolCallId: "early",
      title: "Glob dir",
      kind: "search",
      status: "completed",
    });
    reduce(view, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "看完了" },
    });

    const segments = messageSegments(assistant(view));
    expect(segments.map((segment) => segment.kind)).toEqual([
      "tool",
      "content",
    ]);
    expect(segments[0]!.kind === "tool" && segments[0]!.toolCallId).toBe(
      "early",
    );
  });

  it("c) multiple ACP tools remain available on one assistant timeline", () => {
    const view = fixtureView("explore");
    for (const [toolCallId, title, kind] of [
      ["a", "Read a", "read"],
      ["b", "Grep b", "search"],
      ["c", "Glob c", "search"],
      ["e", "Edit", "edit"],
    ] as const) {
      reduce(view, {
        sessionUpdate: "tool_call",
        toolCallId,
        title,
        kind,
        status: "completed",
      });
    }

    const tools = messageSegments(assistant(view)).filter(
      (segment) => segment.kind === "tool",
    );
    expect(tools).toHaveLength(4);
    expect(
      tools.some(
        (segment) => segment.kind === "tool" && segment.toolKind === "edit",
      ),
    ).toBe(true);
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
    expect(messages.some((message) => message.marker === "turn_end")).toBe(true);

    const latch = armStopLatch(createStopLatchState(), "s1", 0);
    expect(canSendWithStopLatch("streaming", latch)).toBe(false);
    const next = tickStopLatch(latch, "streaming", STOP_LATCH_MS);
    expect(next.forceComplete).toBe(true);
    expect(canSendWithStopLatch("streaming", next.latch)).toBe(true);
  });
});

import { describe, expect, it } from "vitest";
import {
  busySessionIds,
  emptyLiveSnapshot,
  inferTurnProgressFromMessages,
  isSessionLiveBusy,
  markSawModelOutput,
  mergeTurnProgressFromMessages,
  projectHostIntoLiveMap,
  resumeStateForSession,
  upsertLiveSnapshot,
} from "./sessionLiveStore";
import type { ChatMessage } from "./session";

describe("sessionLiveStore", () => {
  it("tracks multi-session busy", () => {
    let map = {};
    map = projectHostIntoLiveMap(map, {
      sessionId: "a",
      state: "streaming",
      streamingMessageId: "m1",
    });
    map = projectHostIntoLiveMap(map, {
      sessionId: "b",
      state: "streaming",
    });
    map = projectHostIntoLiveMap(map, {
      sessionId: "c",
      state: "ready",
    });
    const busy = busySessionIds(map);
    expect(busy.has("a")).toBe(true);
    expect(busy.has("b")).toBe(true);
    expect(busy.has("c")).toBe(false);
    expect(isSessionLiveBusy(map, "a")).toBe(true);
  });

  it("clears live tool when host leaves streaming", () => {
    let map = upsertLiveSnapshot(
      {},
      {
        sessionId: "a",
        state: "streaming",
        liveToolTitle: "Reading x",
        liveToolId: "t1",
      },
    );
    map = projectHostIntoLiveMap(map, { sessionId: "a", state: "ready" });
    expect(map.a!.liveToolTitle).toBeNull();
    expect(map.a!.state).toBe("ready");
  });

  it("empty snapshot defaults", () => {
    const s = emptyLiveSnapshot("x", 1);
    expect(s.sessionId).toBe("x");
    expect(s.state).toBe("idle");
  });

  it("keeps other sessions busy when host focuses a different chat", () => {
    let map = projectHostIntoLiveMap(
      {},
      { sessionId: "a", state: "streaming", streamingMessageId: "m1" },
    );
    // User switches to B (host focus) — A must remain busy in the map.
    map = projectHostIntoLiveMap(map, {
      sessionId: "b",
      state: "ready",
    });
    expect(busySessionIds(map).has("a")).toBe(true);
    expect(busySessionIds(map).has("b")).toBe(false);
    expect(map.a!.state).toBe("streaming");
    expect(map.b!.state).toBe("ready");
  });

  it("re-attaches a background turn when its chat is reopened", () => {
    // A is streaming in background; Host focus sits on B.
    const map = projectHostIntoLiveMap({}, {
      sessionId: "a",
      state: "streaming",
      streamingMessageId: "m-a",
    });
    const live = { sessionId: "b", state: "ready" as const };

    // Opening A must keep the spinner + stream pipeline, not show it as done.
    expect(resumeStateForSession("a", live, map)).toEqual({
      state: "streaming",
      streamingMessageId: "m-a",
    });
    // Host live slot always wins for its own chat.
    expect(resumeStateForSession("b", live, map)).toEqual({
      state: "ready",
      streamingMessageId: null,
    });
    // A chat with no live work opens idle.
    expect(resumeStateForSession("c", live, map)).toEqual({
      state: "idle",
      streamingMessageId: null,
    });
  });

  it("does not resurrect a finished background chat as streaming", () => {
    let map = projectHostIntoLiveMap({}, {
      sessionId: "a",
      state: "streaming",
      streamingMessageId: "m-a",
    });
    map = projectHostIntoLiveMap(map, { sessionId: "a", state: "ready" });
    expect(
      resumeStateForSession("a", { sessionId: null, state: "idle" }, map),
    ).toEqual({ state: "idle", streamingMessageId: null });
  });

  it("keeps sawModelOutput sticky across streaming host projections", () => {
    let map = projectHostIntoLiveMap(
      {},
      { sessionId: "a", state: "streaming", streamingMessageId: "m1" },
    );
    map = markSawModelOutput(map, "a");
    map = projectHostIntoLiveMap(map, {
      sessionId: "a",
      state: "streaming",
      streamingMessageId: "m1",
    });
    expect(map.a!.sawModelOutput).toBe(true);
    // Leaving the turn clears flags.
    map = projectHostIntoLiveMap(map, { sessionId: "a", state: "ready" });
    expect(map.a!.sawModelOutput).toBe(false);
  });

  it("同步记录首个模型输出，不等待定时刷新", () => {
    const initial = projectHostIntoLiveMap(
      {},
      { sessionId: "a", state: "streaming", streamingMessageId: "m1" },
      100,
    );

    const marked = markSawModelOutput(initial, "a", 101);

    expect(initial.a!.sawModelOutput).toBe(false);
    expect(marked.a!.sawModelOutput).toBe(true);
    expect(marked.a!.updatedAt).toBe(101);
  });

  it("infers turn progress from journal after last user message", () => {
    const msgs: ChatMessage[] = [
      { id: "u1", role: "user", content: "hi" },
      { id: "t1", role: "tool", content: "tool_step|completed||read", marker: "tool_step" },
      { id: "a1", role: "assistant", content: "done report" },
    ];
    expect(inferTurnProgressFromMessages(msgs)).toEqual({
      sawModelOutput: true,
      sawToolActivity: true,
    });
    let map = mergeTurnProgressFromMessages({}, "s", msgs);
    expect(map.s!.sawModelOutput).toBe(true);
    expect(map.s!.sawToolActivity).toBe(true);
  });
});

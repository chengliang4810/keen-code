import { describe, expect, it } from "vitest";
import {
  appendSessionHistory,
  draftHistoryEntry,
  EMPTY_SESSION_NAVIGATION_HISTORY,
  historyCanGoBack,
  historyCanGoForward,
  moveSessionHistory,
  removeSessionHistoryAt,
  sessionHistoryEntry,
} from "./sessionNavigationHistory";

const row = (id: string) => ({
  id,
  title: id,
  projectId: null,
  updatedAt: "",
  lastUserMessageAt: null,
  archived: false,
  pinned: false,
});

describe("session navigation history", () => {
  it("does not add consecutive duplicates, including the initial draft", () => {
    let state = EMPTY_SESSION_NAVIGATION_HISTORY;
    const draft = draftHistoryEntry();
    state = appendSessionHistory(state, draft);
    state = appendSessionHistory(state, draft);
    state = appendSessionHistory(state, sessionHistoryEntry(row("a")));
    state = appendSessionHistory(state, sessionHistoryEntry(row("a")));

    expect(state.entries).toHaveLength(2);
    expect(state.index).toBe(1);
  });

  it("truncates the forward branch after a new user navigation", () => {
    let state = EMPTY_SESSION_NAVIGATION_HISTORY;
    state = appendSessionHistory(state, sessionHistoryEntry(row("a")));
    state = appendSessionHistory(state, sessionHistoryEntry(row("b")));
    state = moveSessionHistory(state, "back").state;
    state = appendSessionHistory(state, sessionHistoryEntry(row("c")));

    expect(state.entries.map((entry) => entry.kind === "session" ? entry.sessionId : "draft"))
      .toEqual(["a", "c"]);
    expect(state.index).toBe(1);
    expect(historyCanGoForward(state)).toBe(false);
  });

  it("moves only inside the stack and reports exact boundaries", () => {
    let state = appendSessionHistory(
      EMPTY_SESSION_NAVIGATION_HISTORY,
      sessionHistoryEntry(row("a")),
    );
    state = appendSessionHistory(state, sessionHistoryEntry(row("b")));
    expect(historyCanGoBack(state)).toBe(true);
    expect(historyCanGoForward(state)).toBe(false);

    const back = moveSessionHistory(state, "back");
    expect(back.entry?.kind === "session" && back.entry.sessionId).toBe("a");
    expect(historyCanGoBack(back.state)).toBe(false);
    expect(historyCanGoForward(back.state)).toBe(true);
    expect(moveSessionHistory(back.state, "back").entry).toBeNull();
  });

  it("removes deleted targets without leaving a dangling index", () => {
    let state = EMPTY_SESSION_NAVIGATION_HISTORY;
    state = appendSessionHistory(state, sessionHistoryEntry(row("a")));
    state = appendSessionHistory(state, sessionHistoryEntry(row("b")));
    state = appendSessionHistory(state, sessionHistoryEntry(row("c")));
    state = moveSessionHistory(state, "back").state;
    state = removeSessionHistoryAt(state, 1);

    expect(state.entries.map((entry) => entry.kind === "session" ? entry.sessionId : "draft"))
      .toEqual(["a", "c"]);
    expect(state.index).toBe(1);
    expect(moveSessionHistory(state, "back").entry?.kind).toBe("session");
  });

  it("keeps the original current entry when a back target is deleted", () => {
    let state = EMPTY_SESSION_NAVIGATION_HISTORY;
    state = appendSessionHistory(state, sessionHistoryEntry(row("a")));
    state = appendSessionHistory(state, sessionHistoryEntry(row("b")));
    state = appendSessionHistory(state, sessionHistoryEntry(row("c")));
    const moved = moveSessionHistory(state, "back");
    const repaired = removeSessionHistoryAt(
      { ...moved.state, index: state.index },
      moved.state.index,
    );

    expect(repaired.entries.map((entry) => entry.kind === "session" ? entry.sessionId : "draft"))
      .toEqual(["a", "c"]);
    expect(repaired.index).toBe(1);
  });
});

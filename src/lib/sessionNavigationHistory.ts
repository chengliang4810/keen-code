import type { Project, SessionRow } from "@/features/app/models";

/**
 * 会话导航历史只保存可重新定位视图所需的轻量元数据。
 * 消息、运行状态和持久化会话仍由 Host/ACP 维护，不在这里复制。
 */
export type SessionNavigationHistoryEntry =
  | {
      kind: "session";
      sessionId: string;
      row: SessionRow;
      projectId: string | null;
    }
  | {
      kind: "draft";
      projectId: string | null;
    };

export interface SessionNavigationHistoryState {
  entries: readonly SessionNavigationHistoryEntry[];
  index: number;
}

export const EMPTY_SESSION_NAVIGATION_HISTORY: SessionNavigationHistoryState = {
  entries: [],
  index: -1,
};

export function sessionHistoryEntry(
  row: SessionRow,
  project?: Project | null,
): SessionNavigationHistoryEntry {
  return {
    kind: "session",
    sessionId: row.id,
    row,
    projectId: project?.id ?? row.projectId ?? null,
  };
}

export function draftHistoryEntry(
  project?: Project | null,
): SessionNavigationHistoryEntry {
  return { kind: "draft", projectId: project?.id ?? null };
}

export function sameSessionHistoryEntry(
  left: SessionNavigationHistoryEntry | undefined,
  right: SessionNavigationHistoryEntry,
): boolean {
  if (!left || left.kind !== right.kind) return false;
  if (left.kind === "session" && right.kind === "session") {
    return left.sessionId === right.sessionId;
  }
  return left.kind === "draft" && right.kind === "draft" &&
    left.projectId === right.projectId;
}

/** Adds a user navigation and discards the forward branch. */
export function appendSessionHistory(
  state: SessionNavigationHistoryState,
  entry: SessionNavigationHistoryEntry,
): SessionNavigationHistoryState {
  if (sameSessionHistoryEntry(state.entries[state.index], entry)) return state;

  const entries = state.entries.slice(0, state.index + 1);
  entries.push(entry);
  return { entries, index: entries.length - 1 };
}

export function historyCanGoBack(
  state: SessionNavigationHistoryState,
): boolean {
  return state.index > 0;
}

export function historyCanGoForward(
  state: SessionNavigationHistoryState,
): boolean {
  return state.index >= 0 && state.index < state.entries.length - 1;
}

export function moveSessionHistory(
  state: SessionNavigationHistoryState,
  direction: "back" | "forward",
): { state: SessionNavigationHistoryState; entry: SessionNavigationHistoryEntry | null } {
  const nextIndex = state.index + (direction === "back" ? -1 : 1);
  if (nextIndex < 0 || nextIndex >= state.entries.length) {
    return { state, entry: null };
  }
  return {
    state: { entries: state.entries, index: nextIndex },
    entry: state.entries[nextIndex] ?? null,
  };
}

/** Removes an invalid/deleted target and keeps the current index valid. */
export function removeSessionHistoryAt(
  state: SessionNavigationHistoryState,
  index: number,
): SessionNavigationHistoryState {
  if (index < 0 || index >= state.entries.length) return state;
  const entries = state.entries.filter((_, itemIndex) => itemIndex !== index);
  const nextIndex = entries.length === 0
    ? -1
    : Math.min(
        state.index > index ? state.index - 1 : state.index,
        entries.length - 1,
      );
  return { entries, index: nextIndex };
}

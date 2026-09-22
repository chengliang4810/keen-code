/**
 * Which chat the workbench is showing, plus a monotonic epoch bumped on every
 * 用户导航（打开任务或新建任务）。
 *
 * Why the epoch: `sessionId: null` means "a draft", and **every draft looks
 * identical**. Async work started on one draft (materialize + `sessionConnect`,
 * which can take seconds) would compare `null === null`, conclude the user was
 * still on its own view, and yank the workbench back to the chat it just
 * started — right as the agent began executing. The epoch gives each draft an
 * identity so that comparison fails.
 *
 * Rule for any async path that touches view state: capture the focus **before**
 * the first `await`, and re-check it before writing.
 */
export interface ViewFocus {
  sessionId: string | null;
  epoch: number;
}

/** True when the user has not navigated since `origin` was captured. */
export function isSameView(origin: ViewFocus, current: ViewFocus): boolean {
  return origin.epoch === current.epoch;
}

/**
 * Whether async work that started on `origin` may take the workbench over for
 * `sessionId` (draft materialized into a real chat, or connect resolved).
 *
 * Allowed only when the user is already looking at that chat, or has not
 * navigated since the work started.
 */
export function shouldAdoptView(
  origin: ViewFocus,
  current: ViewFocus,
  sessionId: string,
): boolean {
  if (current.sessionId === sessionId) return true;
  return isSameView(origin, current);
}

/**
 * Whether the user is still watching the thread a send was dispatched for.
 *
 * Real ids compare directly — reopening the same chat still counts as watching
 * it. Drafts have no id, so they must also match the navigation epoch.
 */
export function isViewingSendTarget(
  origin: ViewFocus,
  current: ViewFocus,
  targetSessionId: string | null | undefined,
): boolean {
  if (targetSessionId != null) return current.sessionId === targetSessionId;
  return current.sessionId == null && isSameView(origin, current);
}

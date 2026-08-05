/**
 * Composer 提示词历史：支持 ↑/↓ 回看与 `/history` 选择器。
 *
 * 范围固定为当前 Session，不跨 Session 合并。
 * History is newest-first (index 0 = most recent user message).
 * Index `null` means not browsing (live draft).
 */

export type PromptHistoryStep = {
  /** Index into history (0 = newest), or null when not browsing. */
  index: number | null;
  /** Draft text to apply ("" when leaving history). */
  text: string;
};

/** One row in the `/history` picker (filtered view of session history). */
export type PromptHistoryEntry = {
  /** Index into the unfiltered newest-first history list. */
  historyIndex: number;
  /** Stored prompt text (`[[skill:…]]` form). */
  text: string;
};

/**
 * Extract prior user prompt strings from session messages, newest first.
 * Skips empty / whitespace-only content. Keeps stored display form
 * (`[[skill:…]]` tokens) so the composer can re-render chips.
 */
export function collectUserPromptHistory(
  messages: ReadonlyArray<{ role: string; content?: string | null }>,
): string[] {
  const out: string[] = [];
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (!m || m.role !== "user") continue;
    const c = m.content ?? "";
    if (!c.trim()) continue;
    out.push(c);
  }
  return out;
}

/**
 * Fuzzy-filter current-session prompt history (newest first).
 * Empty query returns every entry. Match is case-insensitive substring.
 */
export function filterPromptHistory(
  history: readonly string[],
  query: string,
): PromptHistoryEntry[] {
  const q = query.trim().toLowerCase();
  const out: PromptHistoryEntry[] = [];
  for (let historyIndex = 0; historyIndex < history.length; historyIndex++) {
    const text = history[historyIndex] ?? "";
    if (q && !text.toLowerCase().includes(q)) continue;
    out.push({ historyIndex, text });
  }
  return out;
}

/**
 * One-line preview for the history list: collapse whitespace/newlines.
 * Caller may pre-map skill tokens (`previewStoredAsSlash`).
 */
export function promptHistoryListPreview(
  text: string,
  maxLen = 120,
): string {
  const flat = text.replace(/\s+/g, " ").trim();
  if (flat.length <= maxLen) return flat;
  return `${flat.slice(0, Math.max(1, maxLen - 1))}…`;
}

/**
 * Compute next history index for ↑ / ↓.
 * - null means "live empty draft" (not browsing)
 * - up from null → 0; up clamps at oldest
 * - down from 0 → null (clear); down from null stays null
 */
export function nextPromptHistoryIndex(
  currentIndex: number | null,
  historyLength: number,
  direction: "up" | "down",
): number | null {
  if (historyLength <= 0) return null;
  if (direction === "up") {
    if (currentIndex == null) return 0;
    return Math.min(currentIndex + 1, historyLength - 1);
  }
  // down
  if (currentIndex == null) return null;
  if (currentIndex <= 0) return null;
  return currentIndex - 1;
}

/**
 * Pure step: given history (newest first) and direction, return next
 * index + text for the composer.
 */
export function stepPromptHistory(
  history: readonly string[],
  currentIndex: number | null,
  direction: "up" | "down",
): PromptHistoryStep {
  const index = nextPromptHistoryIndex(
    currentIndex,
    history.length,
    direction,
  );
  if (index == null) return { index: null, text: "" };
  return { index, text: history[index] ?? "" };
}

/**
 * Whether ↑/↓ should be claimed for history navigation.
 * Parent must ensure slash palette is closed before calling.
 *
 * - ArrowUp: only when draft is empty (start) or already browsing
 * - ArrowDown: only while already browsing (forward / clear)
 */
export function shouldHandlePromptHistoryKey(input: {
  key: string;
  draftEmpty: boolean;
  browsing: boolean;
  historyLength: number;
}): boolean {
  if (input.historyLength <= 0) return false;
  if (input.key !== "ArrowUp" && input.key !== "ArrowDown") return false;
  if (input.key === "ArrowUp") {
    return input.draftEmpty || input.browsing;
  }
  return input.browsing;
}

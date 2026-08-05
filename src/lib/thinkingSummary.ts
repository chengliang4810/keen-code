/**
 * Collapse-label for thinking blocks — CodePilot / Opencode style.
 *
 * Prefer the first **bold** or # heading so the row answers "what is it
 * thinking about?" instead of dumb counters like "思考 1 / 思考 2".
 */

const MAX_SUMMARY_CHARS = 48;

/** Strip light markdown wrappers for a one-line trigger label. */
function cleanLine(s: string): string {
  return s
    .replace(/^#{1,6}\s+/, "")
    .replace(/\*\*(.+?)\*\*/g, "$1")
    .replace(/__(.+?)__/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

function clip(s: string, max = MAX_SUMMARY_CHARS): string {
  if (s.length <= max) return s;
  return `${s.slice(0, Math.max(1, max - 1)).trimEnd()}…`;
}

/**
 * Extract a short collapse summary from thinking markdown.
 * Returns null when nothing useful can be shown (caller falls back to
 * "Thinking…" / "Thought for Ns" / "Reasoning complete").
 */
export function extractThinkingSummary(
  content: string | null | undefined,
): string | null {
  if (!content?.trim()) return null;

  const bold = content.match(/\*\*(.+?)\*\*/);
  if (bold?.[1]?.trim()) return clip(cleanLine(bold[1]));

  const heading = content.match(/^#{1,4}\s+(.+)$/m);
  if (heading?.[1]?.trim()) return clip(cleanLine(heading[1]));

  // First non-empty plain line (skip pure list markers / quotes).
  for (const line of content.split(/\r?\n/)) {
    const t = cleanLine(line);
    if (!t) continue;
    if (/^[-*+>]\s*$/.test(t)) continue;
    // Skip ultra-short noise ("ok", "…").
    if (t.length < 2) continue;
    return clip(t);
  }
  return null;
}

/**
 * Lightweight tool display registry — shared by turn activity + tasks panel.
 * Summaries only; live mid-stream still prefers Host title via toolStepDisplayTitle.
 */

export type ToolDisplayKind =
  | "bash"
  | "read"
  | "edit"
  | "search"
  | "subagent"
  | "fallback";

export interface ToolDisplayInfo {
  kind: ToolDisplayKind;
  /** Short i18n-neutral label (English token; UI may map). */
  shortLabel: string;
  /** One-line summary for lists. */
  summary: string;
  /** True when this kind is "gathering context" (read/list/search). */
  isContext: boolean;
}

function lower(s: string | null | undefined): string {
  return (s || "").toLowerCase().trim().replace(/-/g, "_");
}

/** 把工具名称标准化为当前界面分类使用的稳定键。 */
function normalizedToolName(value: string | null | undefined): string {
  return (value || "").trim().toLowerCase().replace(/[\s./-]+/g, "_");
}

/** 判断工具名称是否属于 Plan/Todo 状态工具。 */
export function isPlanToolName(
  kind: string | null | undefined,
  title?: string | null,
): boolean {
  return [kind, title].some((value) => {
    const name = normalizedToolName(value);
    return (
      name === "todo" ||
      name === "todowrite" ||
      name === "todo_write" ||
      name === "plan" ||
      name === "update_plan"
    );
  });
}

/** 判断工具名称是否属于持久 Goal 状态工具。 */
export function isGoalToolName(
  kind: string | null | undefined,
  title?: string | null,
): boolean {
  return [kind, title].some((value) => {
    const name = normalizedToolName(value);
    return (
      name === "goal" ||
      name === "create_goal" ||
      name === "get_goal" ||
      name === "update_goal" ||
      name === "goal_upsert" ||
      name === "goal_transition" ||
      name === "goal_clear"
    );
  });
}

function basename(path: string): string {
  const p = path.replace(/\\/g, "/");
  const parts = p.split("/").filter(Boolean);
  return parts[parts.length - 1] || path;
}

function clip(s: string, max = 56): string {
  const t = s.trim();
  if (t.length <= max) return t;
  return `${t.slice(0, max - 1).trimEnd()}…`;
}

/** Classify a raw tool kind / title into a display bucket. */
export function classifyToolKind(
  kind: string | null | undefined,
  title?: string | null,
): ToolDisplayKind {
  const k = lower(kind);
  const t = lower(title);
  if (k === "bash" || k === "execute" || t === "bash" || t === "execute") {
    return "bash";
  }
  if (k === "agent" || k === "subagent" || t === "agent" || t === "subagent") {
    return "subagent";
  }
  if (
    k === "write" ||
    k === "edit" ||
    k === "folder_operations" ||
    t === "write" ||
    t === "edit"
  ) {
    return "edit";
  }
  if (k === "grep" || k === "glob" || k === "search" || t === "grep" || t === "glob") {
    return "search";
  }
  if (k === "read" || t === "read") {
    return "read";
  }
  return "fallback";
}

export function isContextToolKind(
  kind: string | null | undefined,
  title?: string | null,
): boolean {
  const c = classifyToolKind(kind, title);
  return c === "read" || c === "search";
}

export function toolShortLabel(kind: ToolDisplayKind): string {
  switch (kind) {
    case "bash":
      return "Shell";
    case "read":
      return "Read";
    case "edit":
      return "Edit";
    case "search":
      return "Search";
    case "subagent":
      return "Agent";
    default:
      return "Tool";
  }
}

/**
 * Human summary for a tool row.
 * Prefer path basename / detail snippet over bare kind.
 */
export function summarizeToolDisplay(input: {
  kind?: string | null;
  title?: string | null;
  detail?: string | null;
  path?: string | null;
}): ToolDisplayInfo {
  const bucket = classifyToolKind(input.kind, input.title);
  const path = (input.path || "").trim();
  const detail = (input.detail || "").trim();
  const title = (input.title || "").trim();
  let summary = "";
  if (path) {
    summary = basename(path);
    if (bucket === "bash" && detail) {
      summary = clip(detail.split("\n")[0] || detail);
    }
  } else if (detail) {
    summary = clip(detail.split("\n")[0] || detail);
  } else if (title && !/^tool$/i.test(title)) {
    summary = clip(title);
  } else if (input.kind) {
    summary = clip(input.kind.replace(/[_./]+/g, " "));
  } else {
    summary = toolShortLabel(bucket);
  }
  return {
    kind: bucket,
    shortLabel: toolShortLabel(bucket),
    summary,
    isContext: bucket === "read" || bucket === "search",
  };
}

/** Last N non-empty lines of tool detail (expanded activity). */
export function toolDetailTail(
  detail: string | null | undefined,
  maxLines = 8,
): string {
  if (!detail?.trim()) return "";
  const lines = detail.replace(/\r\n/g, "\n").split("\n");
  const kept = lines.filter((l, i) => l.trim() || i === lines.length - 1);
  if (kept.length <= maxLines) return kept.join("\n");
  return kept.slice(-maxLines).join("\n");
}

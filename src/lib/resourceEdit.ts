/**
 * Resource pane text edit helpers — which kinds can Save, dirty check, conflict parse.
 */

/** 当前可编辑的 UTF-8 文本预览类型。 */
const EDITABLE_KINDS = new Set([
  "text",
  "code",
  "markdown",
  "json",
  "config",
  "css",
  "csv",
  "html",
]);

/** 判断资源是否可作为完整 UTF-8 文本安全编辑。 */
export function isResourceTextEditable(opts: {
  kind?: string | null;
  text?: string | null;
  truncated?: boolean | null;
  error?: string | null;
}): boolean {
  if (opts.truncated) return false;
  if (opts.error && !opts.text) return false;
  if (opts.text == null) return false;
  const kind = (opts.kind || "text").toLowerCase();
  if (EDITABLE_KINDS.has(kind)) return true;
  // Host may classify odd extensions as generic text with a body.
  if (kind === "binary" || kind === "image" || kind === "video" || kind === "audio") {
    return false;
  }
  if (kind.startsWith("doc") || kind === "xlsx" || kind === "pptx" || kind === "odf") {
    return false;
  }
  // Unknown kind but we have full text → allow.
  return opts.text.length >= 0 && !opts.truncated;
}

export function isResourceDraftDirty(
  draft: string | null | undefined,
  baseline: string | null | undefined,
): boolean {
  if (draft == null) return false;
  if (baseline == null) return draft.length > 0;
  return draft !== baseline;
}

/** Host errors for concurrent disk changes start with this prefix. */
export function isFsWriteConflict(err: unknown): boolean {
  const s = String(err ?? "");
  return s.includes("CONFLICT:") || s.toLowerCase().includes("conflict:");
}

/** Markdown defaults to preview; other editable kinds open in the editor. */
export function defaultResourceEditMode(kind: string | null | undefined): boolean {
  const k = (kind || "").toLowerCase();
  if (k === "markdown") return false;
  return true;
}

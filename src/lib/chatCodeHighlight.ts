import hljs from "highlight.js/lib/core";
import {
  ensureHighlightLanguages,
  normalizeHighlightLanguage,
} from "@/lib/highlightLanguages";
/** Highlight an explicitly labelled fence; unknown/plain text stays cheap. */
export function highlightChatCode(
  code: string,
  language: string | undefined,
): string | null {
  const raw = (language ?? "").trim().toLowerCase();
  if (!raw || raw === "text" || raw === "plaintext" || raw === "txt") {
    return null;
  }
  ensureHighlightLanguages();
  const normalized = normalizeHighlightLanguage(raw);
  if (!hljs.getLanguage(normalized)) return null;
  try {
    return hljs.highlight(code, {
      language: normalized,
      ignoreIllegals: true,
    }).value;
  } catch {
    return null;
  }
}

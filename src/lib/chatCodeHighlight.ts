import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import go from "highlight.js/lib/languages/go";
import java from "highlight.js/lib/languages/java";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdown from "highlight.js/lib/languages/markdown";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

let registered = false;

const aliases: Record<string, string> = {
  cjs: "javascript",
  html: "xml",
  js: "javascript",
  jsx: "javascript",
  md: "markdown",
  mjs: "javascript",
  py: "python",
  rs: "rust",
  sh: "bash",
  shell: "bash",
  ts: "typescript",
  tsx: "typescript",
  yml: "yaml",
};

function ensureLanguages(): void {
  if (registered) return;
  registered = true;
  const languages: [string, typeof javascript][] = [
    ["bash", bash],
    ["css", css],
    ["diff", diff],
    ["go", go],
    ["java", java],
    ["javascript", javascript],
    ["json", json],
    ["markdown", markdown],
    ["python", python],
    ["rust", rust],
    ["sql", sql],
    ["typescript", typescript],
    ["xml", xml],
    ["yaml", yaml],
  ];
  for (const [name, definition] of languages) {
    if (!hljs.getLanguage(name)) hljs.registerLanguage(name, definition);
  }
}
/** Highlight an explicitly labelled fence; unknown/plain text stays cheap. */
export function highlightChatCode(
  code: string,
  language: string | undefined,
): string | null {
  const raw = (language ?? "").trim().toLowerCase();
  if (!raw || raw === "text" || raw === "plaintext" || raw === "txt") {
    return null;
  }
  ensureLanguages();
  const normalized = aliases[raw] ?? raw;
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

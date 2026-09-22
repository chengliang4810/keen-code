import { normalizeHighlightLanguage } from "@/lib/highlightLanguages";

export type CodeBlockIconKind =
  | "code"
  | "data"
  | "diff"
  | "document"
  | "diagram"
  | "shell";

export interface CodeBlockDescriptor {
  /** Stable display name used when a fence does not carry a real filename. */
  fileName: string;
  /** Normalized language id used by highlight.js and mermaid detection. */
  language: string;
  iconKind: CodeBlockIconKind;
}

const RAW_FILE_NAMES: Readonly<Record<string, string>> = {
  bash: "script.sh",
  c: "main.c",
  cpp: "main.cpp",
  cjs: "index.cjs",
  conf: "config.conf",
  css: "styles.css",
  csharp: "Program.cs",
  diff: "changes.diff",
  dockerfile: "Dockerfile",
  fish: "script.fish",
  go: "main.go",
  graphql: "schema.graphql",
  h: "main.h",
  hpp: "main.hpp",
  html: "index.html",
  ini: "config.ini",
  java: "Main.java",
  javascript: "index.js",
  js: "index.js",
  jsx: "index.jsx",
  json: "data.json",
  jsonc: "data.jsonc",
  kotlin: "Main.kt",
  lua: "script.lua",
  makefile: "Makefile",
  markdown: "README.md",
  md: "README.md",
  mdx: "README.mdx",
  mjs: "index.mjs",
  mts: "index.mts",
  php: "index.php",
  plaintext: "text.txt",
  proto: "schema.proto",
  protobuf: "schema.proto",
  py: "main.py",
  python: "main.py",
  r: "script.r",
  rb: "main.rb",
  ruby: "main.rb",
  rs: "main.rs",
  rust: "main.rs",
  scss: "styles.scss",
  sh: "script.sh",
  shell: "script.sh",
  sql: "query.sql",
  svg: "image.svg",
  swift: "main.swift",
  text: "text.txt",
  toml: "config.toml",
  ts: "index.ts",
  tsx: "index.tsx",
  typescript: "index.ts",
  txt: "text.txt",
  xml: "index.xml",
  yaml: "config.yaml",
  yml: "config.yml",
  zsh: "script.zsh",
};

const ICON_KIND_BY_LANGUAGE: Readonly<Record<string, CodeBlockIconKind>> = {
  bash: "shell",
  c: "code",
  cpp: "code",
  csharp: "code",
  css: "code",
  diff: "diff",
  dockerfile: "shell",
  go: "code",
  graphql: "code",
  html: "code",
  ini: "data",
  java: "code",
  javascript: "code",
  json: "data",
  kotlin: "code",
  lua: "code",
  makefile: "shell",
  markdown: "document",
  mermaid: "diagram",
  php: "code",
  plaintext: "document",
  protobuf: "data",
  python: "code",
  r: "code",
  ruby: "code",
  rust: "code",
  scss: "code",
  sql: "data",
  swift: "code",
  typescript: "code",
  xml: "data",
  yaml: "data",
};

function rawLanguage(language: string | undefined): string {
  return (language ?? "text")
    .trim()
    .toLowerCase()
    .replace(/^language-/, "") || "text";
}

/** Resolve a compact file identity for a fence that has no filename metadata. */
export function resolveCodeBlockDescriptor(
  language: string | undefined,
): CodeBlockDescriptor {
  const raw = rawLanguage(language);
  const normalized = normalizeHighlightLanguage(raw) || "plaintext";
  const languageForDisplay =
    raw === "mermaid" || raw === "mmd"
      ? "mermaid"
      : raw === "text"
        ? "plaintext"
        : normalized;
  const fileName =
    RAW_FILE_NAMES[raw] ??
    RAW_FILE_NAMES[normalized] ??
    (raw === "mermaid" || raw === "mmd"
      ? "diagram.mmd"
      : /^[a-z0-9+#-]+$/i.test(raw)
        ? `snippet.${raw}`
        : "snippet.txt");

  return {
    fileName,
    language: languageForDisplay,
    iconKind: ICON_KIND_BY_LANGUAGE[languageForDisplay] ?? "code",
  };
}

export function isMermaidLanguage(language: string | undefined): boolean {
  const raw = rawLanguage(language);
  return raw === "mermaid" || raw === "mmd";
}

/**
 * Keep inference deliberately narrow: an unlabeled text fence must not turn
 * ordinary prose beginning with a Mermaid-like word into an empty diagram.
 */
export function shouldRenderMermaidCodeBlock(
  language: string | undefined,
  code: string,
): boolean {
  if (isMermaidLanguage(language)) return code.trim().length > 0;
  if (language && rawLanguage(language) !== "text") return false;
  const firstLine = code.trimStart().split(/\r?\n/, 1)[0]?.trim() ?? "";
  return /^(?:graph|flowchart|sequenceDiagram|classDiagram|stateDiagram(?:-v2)?|erDiagram|journey|gantt|pie|quadrantChart|gitGraph|mindmap|timeline)\b/i.test(
    firstLine,
  );
}

type OpenTag = { name: string; source: string };

function tagName(source: string): string | null {
  const match = /^<\/?\s*([a-z][\w:-]*)/i.exec(source);
  return match?.[1]?.toLowerCase() ?? null;
}

function isClosingTag(source: string): boolean {
  return /^<\//.test(source);
}

function isSelfClosingTag(source: string): boolean {
  return /\/\s*>$/.test(source) || /^<!/.test(source) || /^<\?/.test(source);
}

/**
 * Split highlight.js HTML into line wrappers without leaving syntax spans
 * open across sibling elements. This keeps line focus/mark styles reliable
 * while preserving tokens that span a newline.
 */
export function splitHighlightedHtml(html: string): string[] {
  const lines = [""];
  const openTags: OpenTag[] = [];
  const tokens = html.match(/<!--[\s\S]*?-->|<\/?[a-z][^>]*>|[^<]+/gi) ?? [];

  const closeOpenTags = () => {
    for (let index = openTags.length - 1; index >= 0; index -= 1) {
      lines[lines.length - 1] += `</${openTags[index]!.name}>`;
    }
  };

  for (const token of tokens) {
    if (!token.startsWith("<")) {
      const parts = token.split("\n");
      for (const [index, part] of parts.entries()) {
        lines[lines.length - 1] += part;
        if (index < parts.length - 1) {
          closeOpenTags();
          lines.push(openTags.map((tag) => tag.source).join(""));
        }
      }
      continue;
    }

    const name = tagName(token);
    if (!name || isSelfClosingTag(token)) {
      lines[lines.length - 1] += token;
      continue;
    }

    if (isClosingTag(token)) {
      lines[lines.length - 1] += token;
      let openIndex = -1;
      for (let index = openTags.length - 1; index >= 0; index -= 1) {
        if (openTags[index]!.name === name) {
          openIndex = index;
          break;
        }
      }
      if (openIndex >= 0) openTags.splice(openIndex, 1);
      continue;
    }

    lines[lines.length - 1] += token;
    openTags.push({ name, source: token });
  }

  closeOpenTags();
  return lines.length > 0 ? lines : [""];
}

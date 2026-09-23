import { normalizeHighlightLanguage } from "@/lib/languageAliases";

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
  /** 展示用规范语言名(别名映射见 lib/languageAliases)与 mermaid 检测共用。 */
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
 * Mermaid 检测(语言显式标注或对空/text 围栏做首行关键字窄识别)。
 * 高亮 token 的拆行逻辑随 highlight.js 迁移到 Shiki 的逐行 token 模型,
 * 见 lib/shikiChatHighlighter.ts。
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

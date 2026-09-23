/**
 * 代码围栏/文件名到规范语言名的共享别名表。
 *
 * 原先内嵌在 `lib/highlightLanguages.ts`(highlight.js 语言注册表)中;
 * 渲染栈迁移到 Shiki 后,语言解析由 shiki 的 `bundledLanguages`/alias 完成,
 * 这里只保留界面展示与文件名归一所需的纯映射。
 */

/** File/fence spellings accepted by both chat fences and resource previews. */
export const LANGUAGE_ALIASES: Readonly<Record<string, string>> = {
  bash: "bash",
  cjs: "javascript",
  conf: "ini",
  fish: "bash",
  h: "c",
  hpp: "cpp",
  html: "xml",
  htm: "xml",
  js: "javascript",
  jsx: "javascript",
  jsonc: "json",
  less: "css",
  md: "markdown",
  mdx: "markdown",
  mjs: "javascript",
  mts: "typescript",
  cts: "typescript",
  py: "python",
  rs: "rust",
  sh: "bash",
  shell: "bash",
  svg: "xml",
  text: "plaintext",
  toml: "ini",
  ts: "typescript",
  tsx: "typescript",
  txt: "plaintext",
  yml: "yaml",
  zsh: "bash",
};

/** Map a fence/filename spelling to its canonical display language. */
export function normalizeHighlightLanguage(
  language: string | undefined,
): string {
  const raw = (language ?? "").trim().toLowerCase().replace(/^language-/, "");
  if (!raw) return "";
  return LANGUAGE_ALIASES[raw] ?? raw;
}

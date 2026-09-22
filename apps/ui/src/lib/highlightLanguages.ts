import hljs from "highlight.js/lib/core";
import type { LanguageFn } from "highlight.js";

import bash from "highlight.js/lib/languages/bash";
import c from "highlight.js/lib/languages/c";
import cpp from "highlight.js/lib/languages/cpp";
import csharp from "highlight.js/lib/languages/csharp";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import dockerfile from "highlight.js/lib/languages/dockerfile";
import go from "highlight.js/lib/languages/go";
import graphql from "highlight.js/lib/languages/graphql";
import ini from "highlight.js/lib/languages/ini";
import java from "highlight.js/lib/languages/java";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import kotlin from "highlight.js/lib/languages/kotlin";
import lua from "highlight.js/lib/languages/lua";
import makefile from "highlight.js/lib/languages/makefile";
import markdown from "highlight.js/lib/languages/markdown";
import php from "highlight.js/lib/languages/php";
import plaintext from "highlight.js/lib/languages/plaintext";
import protobuf from "highlight.js/lib/languages/protobuf";
import python from "highlight.js/lib/languages/python";
import r from "highlight.js/lib/languages/r";
import ruby from "highlight.js/lib/languages/ruby";
import rust from "highlight.js/lib/languages/rust";
import scss from "highlight.js/lib/languages/scss";
import sql from "highlight.js/lib/languages/sql";
import swift from "highlight.js/lib/languages/swift";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

const LANGUAGE_DEFINITIONS: ReadonlyArray<readonly [string, LanguageFn]> = [
  ["bash", bash],
  ["c", c],
  ["cpp", cpp],
  ["csharp", csharp],
  ["css", css],
  ["diff", diff],
  ["dockerfile", dockerfile],
  ["go", go],
  ["graphql", graphql],
  ["ini", ini],
  ["java", java],
  ["javascript", javascript],
  ["json", json],
  ["kotlin", kotlin],
  ["lua", lua],
  ["makefile", makefile],
  ["markdown", markdown],
  ["php", php],
  ["plaintext", plaintext],
  ["protobuf", protobuf],
  ["python", python],
  ["r", r],
  ["ruby", ruby],
  ["rust", rust],
  ["scss", scss],
  ["sql", sql],
  ["swift", swift],
  ["typescript", typescript],
  ["xml", xml],
  ["yaml", yaml],
];

/** File/fence spellings accepted by both chat fences and resource previews. */
export const HIGHLIGHT_LANGUAGE_ALIASES: Readonly<Record<string, string>> = {
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

let registered = false;

/** Register the small, shared highlight.js language set once per core instance. */
export function ensureHighlightLanguages(): void {
  if (registered) return;
  registered = true;

  for (const [name, definition] of LANGUAGE_DEFINITIONS) {
    if (!hljs.getLanguage(name)) hljs.registerLanguage(name, definition);
  }
  for (const [alias, languageName] of Object.entries(
    HIGHLIGHT_LANGUAGE_ALIASES,
  )) {
    if (!hljs.getLanguage(alias) && hljs.getLanguage(languageName)) {
      hljs.registerAliases(alias, { languageName });
    }
  }
}

export function normalizeHighlightLanguage(
  language: string | null | undefined,
): string {
  const raw = (language ?? "")
    .trim()
    .toLowerCase()
    .replace(/^language-/, "");
  return HIGHLIGHT_LANGUAGE_ALIASES[raw] ?? raw;
}

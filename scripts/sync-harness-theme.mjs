import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const revision = "d347e703908d0406b7a7ef80e3a0e594d86b2215";
const source = process.argv[2];
if (!source) throw new Error("请传入 deepseek-harness 源码目录");
const actual = execFileSync("git", ["-C", source, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
if (actual !== revision) throw new Error(`参考版本不匹配：需要 ${revision}，实际 ${actual}`);

const target = fileURLToPath(new URL("../src/styles/harness/", import.meta.url));
mkdirSync(target, { recursive: true });
for (const file of ["base.css", "design-platform.css", "gradient-shadow-text.css"]) {
  // 从固定提交读取，避免参考检出目录的未提交修改悄悄改变生成结果。
  const content = execFileSync("git", ["-C", source, "show", `${revision}:packages/client/ui-theme/src/styles/${file}`], { encoding: "utf8" });
  // 只适配主题宿主选择器，所有原始字体、色值、阴影和排版数值原样保留。
  // KeenCode 在首帧前把 data-theme 写到 html，浮层 portal 也继承同一份令牌。
  const adapted = content.replaceAll("body[data-ds-dark-theme]", '[data-theme="dark"]').replace(/\bbody\b/g, ":root");
  writeFileSync(resolve(target, file), `/* Copyright (c) 2026 DeepSeek. MIT License.
 * Source: deepseek-ai/deepseek-harness @ ${revision}
 * Selector adaptation only. Regenerate with scripts/sync-harness-theme.mjs.
 * Full license: ../../../THIRD_PARTY_NOTICES.md (repository root).
 */\n${adapted.replaceAll("\r\n", "\n")}`);
}
console.log(`已同步 Harness 主题源码 ${revision}`);

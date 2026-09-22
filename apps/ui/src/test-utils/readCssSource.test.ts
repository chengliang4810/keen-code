import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { readSource } from "./readCssSource";

describe("readSource 源码 fixture", () => {
  it("URL 与路径读取同一稳定源码，并按入口顺序展开 CSS 导入", () => {
    const entryUrl = new URL("../styles/app.css", import.meta.url);
    const viaUrl = readSource(entryUrl);
    const viaPath = readSource(fileURLToPath(entryUrl));

    expect(viaPath).toBe(viaUrl);
    expect(viaUrl).not.toContain('@import "./app-foundation.css"');
    expect(viaUrl.indexOf(".sidebar {")).toBeLessThan(
      viaUrl.indexOf(".composer-goal-chip,"),
    );
  });

  it("临时文件绕过缓存并报告完整 CSS 导入循环", () => {
    const fixtureDirectory = mkdtempSync(join(tmpdir(), "keencode-css-source-"));
    const entryPath = join(fixtureDirectory, "entry.css");
    const nestedPath = join(fixtureDirectory, "nested.css");
    try {
      writeFileSync(
        entryPath,
        '.entry-before {}\n@import url("./nested.css");\n.entry-after {}\n',
      );
      writeFileSync(nestedPath, ".nested-first {}\n");
      const first = readSource(entryPath, { cache: false });
      expect(first).toMatch(
        /\.entry-before \{\}[\s\S]*\.nested-first \{\}[\s\S]*\.entry-after \{\}/,
      );

      writeFileSync(nestedPath, ".nested-updated {}\n");
      expect(readSource(entryPath, { cache: false })).toContain(
        ".nested-updated {}",
      );

      writeFileSync(nestedPath, '@import "./entry.css";\n');
      expect(() => readSource(entryPath, { cache: false })).toThrow(
        /检测到 CSS @import 循环:.*entry\.css.*nested\.css.*entry\.css/,
      );
    } finally {
      rmSync(fixtureDirectory, { recursive: true, force: true });
    }
  });
});

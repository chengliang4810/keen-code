import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const styles = readFileSync(new URL("./app-resource.css", import.meta.url), "utf8");

describe("原生滚动条样式契约", () => {
  it("使用 ZCode 的全局 14px 透明轨道与边框色 thumb", () => {
    expect(styles).toMatch(
      /\*\s*\{\s*scrollbar-width:\s*auto;\s*scrollbar-color:\s*var\(--border-subtle\) transparent;/,
    );
    expect(styles).toMatch(
      /\*::\-webkit\-scrollbar\s*\{\s*width:\s*14px;\s*height:\s*14px;/,
    );
    expect(styles).toMatch(
      /\*::\-webkit\-scrollbar-thumb\s*\{[\s\S]*?min-height:\s*32px;[\s\S]*?min-width:\s*32px;[\s\S]*?border:\s*3px solid transparent;[\s\S]*?background:\s*var\(--border-subtle\);[\s\S]*?background-clip:\s*padding-box;/,
    );
    expect(styles).toMatch(/\*::\-webkit\-scrollbar-track\s*\{\s*background:\s*transparent;/);
    expect(styles).toMatch(/\*::\-webkit\-scrollbar-corner\s*\{\s*background:\s*transparent;/);
  });

  it("只为 xterm 与固定格式预览保留显式隐藏", () => {
    expect(styles).toMatch(
      /\.terminal-instance \.xterm-viewport\s*\{\s*scrollbar-width:\s*none;/,
    );
    expect(styles).toMatch(
      /\.terminal-instance \.xterm-viewport::\-webkit\-scrollbar\s*\{[\s\S]*?display:\s*none;[\s\S]*?width:\s*0;[\s\S]*?height:\s*0;/,
    );
    expect(styles).toMatch(
      /\[data-zcode-pptx-render-surface\] \*\s*\{\s*scrollbar-width:\s*none;/,
    );
    expect(styles).not.toContain("*:not(textarea)");
    expect(styles).not.toMatch(/\.settings-page__(?:nav-inner|content)[^{]*\{[^}]*scrollbar-/);
    expect(styles).not.toMatch(/\.prov-rail[^{]*\{[^}]*scrollbar-/);
    expect(styles).not.toMatch(/\.composer__input[^{]*\{[^}]*scrollbar-/);
  });
});

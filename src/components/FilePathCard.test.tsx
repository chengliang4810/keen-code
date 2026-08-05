import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { FilePathCard } from "./FilePathCard";

const labels = {
  open: "打开",
  reveal: "显示",
  copyPath: "复制路径",
};

describe("FilePathCard", () => {
  it("将文件引用渲染为行内主题链接", () => {
    const html = renderToString(
      <FilePathCard path="package.json" labels={labels} />,
    );

    expect(html).toContain('class="file-path-link"');
    expect(html).toContain('class="file-path-link__main"');
    expect(html).toContain('class="file-path-link__name">package.json</span>');
    expect(html).not.toContain('class="file-path-card"');
  });

  it("使用聊天主题色并保留清晰的键盘焦点", () => {
    const css = readFileSync(
      new URL("../styles/app.css", import.meta.url),
      "utf8",
    );
    const linkRule = css.match(/\.file-path-link__main\s*\{([^}]*)\}/)?.[1];
    const wrapperRule = css.match(/\.file-path-link\s*\{([^}]*)\}/)?.[1];
    const focusRule = css.match(
      /\.file-path-link__main:focus-visible\s*\{([^}]*)\}/,
    )?.[1];

    expect(linkRule).toMatch(/color:\s*var\(--chat-link\)/);
    expect(linkRule).toMatch(/background:\s*transparent/);
    expect(linkRule).toMatch(/border:\s*0/);
    expect(wrapperRule).toMatch(/display:\s*inline/);
    expect(linkRule).toMatch(/vertical-align:\s*baseline/);
    expect(css).toMatch(
      /\.file-path-link__icon\s*\{[^}]*vertical-align:\s*text-bottom/s,
    );
    expect(focusRule).toMatch(/outline:/);
  });
});

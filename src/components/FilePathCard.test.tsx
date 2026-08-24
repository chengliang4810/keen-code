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
  it("未确认存在的文件只渲染为普通文字", () => {
    const html = renderToString(
      <FilePathCard path="package.json" labels={labels} />,
    );

    expect(html).toBe("<span>package.json</span>");
    expect(html).not.toContain("file-path-link");
    expect(html).not.toContain("button");
  });

  it("目录 URL 显示完整地址，文件 URL 只显示文件名", () => {
    const directoryUrl = "https://github.com/anthropics/claude-plugins-community/tree/main";
    const fileUrl = "https://github.com/anthropics/claude-plugins-community/blob/main/eli5/plugin.json";
    const directoryHtml = renderToString(
      <FilePathCard path={directoryUrl} kind="url" labels={labels} />,
    );
    const fileHtml = renderToString(
      <FilePathCard path={fileUrl} kind="url" labels={labels} />,
    );

    expect(directoryHtml).toContain(
      `class="file-path-link__name">${directoryUrl}</span>`,
    );
    expect(fileHtml).toContain(
      'class="file-path-link__name">plugin.json</span>',
    );
    expect(fileHtml).not.toContain("disabled");
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
    expect(wrapperRule).toMatch(/display:\s*inline-block/);
    expect(linkRule).toMatch(/display:\s*inline-flex/);
    expect(linkRule).toMatch(/align-items:\s*center/);
    expect(linkRule).toMatch(/vertical-align:\s*baseline/);
    expect(focusRule).toMatch(/outline:/);
    expect(css).toMatch(
      /\.file-path-link__name\s*\{[^}]*overflow-wrap:\s*anywhere;[^}]*white-space:\s*normal;/s,
    );
    expect(css).toMatch(
      /\.chat-md ul > li:has\(\.file-path-link\)::before\s*\{[^}]*top:\s*0\.6em;[^}]*width:\s*5px;[^}]*height:\s*5px;/s,
    );
  });

  it("通过桌面命令使用系统默认浏览器打开 URL", () => {
    const source = readFileSync(
      new URL("./FilePathCard.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("await api.urlOpen(path)");
    expect(source).toContain("isUrl ? openExternal() : openInPanel()");
    expect(source).not.toContain("window.open(path");
  });

  it("解析不到文件时不再把原始路径交给资源栏重试", () => {
    const source = readFileSync(
      new URL("./FilePathCard.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("if (!abs) return;");
    expect(source).toContain(
      "if (!isUrl && !resolvedAbs) return <span>{name}</span>",
    );
    expect(source).not.toContain("Still open with original token");
  });
});

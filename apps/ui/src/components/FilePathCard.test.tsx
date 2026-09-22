import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { FilePathCard } from "./FilePathCard";
import { readCssSource } from "../test-utils/readCssSource";

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
    const directoryUrl = "https://github.com/example/keencode-plugins/tree/main";
    const fileUrl = "https://github.com/example/keencode-plugins/blob/main/demo/plugin.json";
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
    expect(fileHtml).not.toMatch(/<button[^>]*\sdisabled(?:=|\s|>)/);
  });

  it("Markdown 自定义链接文本覆盖显示名，但文件类型图标仍依据真实路径", () => {
    const fileUrl = "https://github.com/example/keencode-plugins/blob/main/demo/plugin.json";
    const html = renderToString(
      <FilePathCard
        path={fileUrl}
        displayName="插件配置"
        kind="url"
        labels={labels}
      />,
    );

    expect(html).toContain('class="file-path-link__name">插件配置</span>');
    expect(html).not.toContain('class="file-path-link__name">plugin.json</span>');
    const source = readFileSync(
      new URL("./FilePathCard.tsx", import.meta.url),
      "utf8",
    );
    expect(source).toContain("<FileTypeIcon name={path} size={16} />");
  });

  it("保留行内路径布局并由 Appica Button 管理视觉状态", () => {
    const css = readCssSource(new URL("../styles/app.css", import.meta.url));
    const linkRule = css.match(/\.file-path-link__main\s*\{([^}]*)\}/)?.[1];
    const wrapperRule = css.match(/\.file-path-link\s*\{([^}]*)\}/)?.[1];
    expect(wrapperRule).toMatch(/display:\s*inline-block/);
    expect(linkRule).toMatch(/display:\s*inline-flex/);
    expect(linkRule).toMatch(/align-items:\s*baseline/);
    expect(css).toMatch(/\.file-path-link__icon\s*\{[^}]*align-self:\s*center;/s);
    expect(linkRule).toMatch(/vertical-align:\s*baseline/);
    expect(linkRule).not.toMatch(/(?:color|background|border|outline):/);
    expect(css).not.toMatch(/\.file-path-link__main:(?:hover|focus-visible|disabled)/);
    expect(css).toMatch(
      /\.file-path-link__name\s*\{[^}]*overflow-wrap:\s*anywhere;[^}]*white-space:\s*normal;/s,
    );
    expect(css).toMatch(
      /\.chat-md ul > li:has\(\.file-path-link\)::before\s*\{[^}]*top:\s*8px;[^}]*width:\s*5px;[^}]*height:\s*5px;/s,
    );
  });

  it("URL 主点击在右侧面板打开，无面板宿主时回退系统浏览器", () => {
    const source = readFileSync(
      new URL("./FilePathCard.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("onClick={() => void openInPanel()}");
    expect(source).toContain("onOpenInPanel({ type: \"url\", url: path, title: name })");
    expect(source).toContain("await openExternal();");
    expect(source).toContain("await api.urlOpen(path)");
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

  it("复制路径把解析等待放进手势内的写入，避免 WebKit 拒绝剪贴板访问", () => {
    const source = readFileSync(
      new URL("./FilePathCard.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("copyTextInGesture(");
    expect(source).not.toContain("navigator.clipboard.writeText");
  });
});

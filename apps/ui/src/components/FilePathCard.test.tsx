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

    // 完整地址留在 title 与 href 上，链接文字只显示文件名。
    expect(directoryHtml).toContain(`title="${directoryUrl}"`);
    expect(directoryHtml).toContain(`>${directoryUrl}</a>`);
    expect(fileHtml).toContain(">plugin.json</a>");
    expect(fileHtml).not.toContain("aria-disabled");
  });

  it("Markdown 自定义链接文本覆盖显示名，且链接不带前置图标", () => {
    const fileUrl = "https://github.com/example/keencode-plugins/blob/main/demo/plugin.json";
    const html = renderToString(
      <FilePathCard
        path={fileUrl}
        displayName="插件配置"
        kind="url"
        labels={labels}
      />,
    );

    expect(html).toContain(">插件配置</a>");
    expect(html).not.toContain(">plugin.json</a>");
    // 超链接形态：纯文字，无图标节点、无内层 meta 盒。
    expect(html).not.toContain("<svg");
    expect(html).not.toContain("file-path-link__icon");
    expect(html).not.toContain("file-path-link__name");
  });

  it("锚点提供原生链接语义，点击一律拦截后由资源面板打开", () => {
    const html = renderToString(
      <FilePathCard
        path="https://github.com/example/repo/blob/main/plugin.json"
        kind="url"
        labels={labels}
      />,
    );
    // 主 webview 不能被外链顶掉：新标签 + 隔离 opener。
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noreferrer noopener"');

    const source = readFileSync(
      new URL("./FilePathCard.tsx", import.meta.url),
      "utf8",
    );
    // 本地 file:// 只用于悬浮预览，左键与中键都不允许触发 webview 导航。
    expect(source).toContain("e.preventDefault()");
    expect(source).toContain("if (e.button !== 0) e.preventDefault()");
    expect(source).toContain("pathToFileUrl(resolvedAbs || path)");
  });

  it("呈现为正文超链接而非按钮盒：无悬浮底色、无阴影", () => {
    const css = readCssSource(new URL("../styles/app.css", import.meta.url));
    const linkRule = css.match(/\.file-path-link\s*\{([^}]*)\}/)?.[1];
    expect(linkRule).toMatch(/color:\s*var\(--chat-link\);/);
    expect(linkRule).toMatch(/text-decoration:\s*underline;/);
    expect(linkRule).toMatch(/text-decoration-style:\s*dotted;/);
    expect(linkRule).not.toMatch(/(?:background|box-shadow|border-radius|height|padding|display):/);
    expect(css).toMatch(
      /\.file-path-link:hover,\s*\.file-path-link:focus-visible\s*\{[^}]*color:\s*var\(--chat-link-hover\);/s,
    );
    // 链接已是普通行内文字，不再需要为它改写段落与列表标记几何。
    expect(css).not.toMatch(/:has\(\.file-path-link\)/);
    // 旧的按钮胶囊结构整体退役。
    expect(css).not.toMatch(/\.file-path-link__(?:main|icon|meta|name)\b/);
  });

  it("URL 主点击在右侧面板打开，无面板宿主时回退系统浏览器", () => {
    const source = readFileSync(
      new URL("./FilePathCard.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("void openInPanel();");
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

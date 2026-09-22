import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { MarkdownChat } from "./MarkdownChat";
import { CodeBlock } from "./CodeBlock";
import {
  resolveCodeBlockDescriptor,
  shouldRenderMermaidCodeBlock,
  splitHighlightedHtml,
} from "./codeBlockMeta";

describe("chat code block", () => {
  it("resolves a representative fence to a filename and file icon kind", () => {
    expect(resolveCodeBlockDescriptor("ts")).toMatchObject({
      fileName: "index.ts",
      language: "typescript",
      iconKind: "code",
    });
    expect(resolveCodeBlockDescriptor("sql")).toMatchObject({
      fileName: "query.sql",
      iconKind: "data",
    });
    expect(resolveCodeBlockDescriptor("mmd")).toMatchObject({
      fileName: "diagram.mmd",
      language: "mermaid",
      iconKind: "diagram",
    });
  });

  it("keeps syntax spans balanced when a highlighted token crosses a line", () => {
    const lines = splitHighlightedHtml('<span class="hljs-string">one\ntwo</span>');
    expect(lines).toEqual([
      '<span class="hljs-string">one</span>',
      '<span class="hljs-string">two</span>',
    ]);
  });

  it("renders line metadata and focused/marked line state without changing code text", () => {
    const html = renderToString(
      <CodeBlock
        language="ts"
        highlight
        focusedRange={{ startLine: 2, endLine: 2 }}
        markedLines={[3]}
      >
        {"const one = 1;\nconst two = 2;\nconst three = 3;"}
      </CodeBlock>,
    );

    expect(html).toContain('data-line="1"');
    expect(html).toContain('data-line="3"');
    expect(html).toContain('data-focused="true"');
    expect(html).toContain('data-marked="true"');
    expect(html).not.toContain("chat-code__filename");
    expect(html).not.toContain("index.ts");
    expect(html).toContain('data-code-block-icon="code"');
    expect(html).toContain("typescript");
    expect(html).toContain("hljs-number");
    expect(html).toContain("three =");
  });

  it("matches the ZCode code block spacing and feedback timing", () => {
    const css = readFileSync(new URL("./lobe-chat.css", import.meta.url), "utf8");
    const source = readFileSync(new URL("./CodeBlock.tsx", import.meta.url), "utf8");

    expect(css).toMatch(/\.chat-code\s*\{[^}]*margin:\s*16px 0;/s);
    expect(css).toMatch(/\.chat-code__bar\s*\{[^}]*padding:\s*8px 12px;/s);
    expect(css).toMatch(
      /\.chat-code__pre\s*\{[^}]*font-size:\s*var\(--text-xs\);/s,
    );
    expect(css).toMatch(/\.chat-code__line\s*\{[^}]*padding:\s*0 12px;/s);
    expect(css).not.toContain(".chat-code__line::before");
    expect(source).toContain("timeout={2000}");
  });

  it("does not start Mermaid rendering for a streaming fence", () => {
    const markdown = "```mermaid\ngraph LR\n  A --> B\n```";
    const html = renderToString(<MarkdownChat streaming>{markdown}</MarkdownChat>);

    expect(html).not.toContain("chat-mermaid");
    expect(html).not.toContain("diagram.mmd");
    expect(html).toContain("mermaid");
    expect(html).toContain('data-line="1"');
  });

  it("recognizes explicit Mermaid fences and a narrow unlabeled graph form", () => {
    expect(shouldRenderMermaidCodeBlock("mermaid", "graph LR\nA-->B")).toBe(true);
    expect(shouldRenderMermaidCodeBlock("text", "graph LR\nA-->B")).toBe(true);
    expect(shouldRenderMermaidCodeBlock("text", "graphical explanation")).toBe(false);
    expect(shouldRenderMermaidCodeBlock("typescript", "graph LR\nA-->B")).toBe(false);
  });

  it("keeps markdown body and heading metrics aligned with the message renderer", () => {
    const css = readFileSync(new URL("./lobe-chat.css", import.meta.url), "utf8");
    const html = renderToString(<MarkdownChat>{"# Heading\n\nBody"}</MarkdownChat>);

    expect(html).toContain("<h1>Heading</h1>");
    expect(css).toMatch(/\.chat-md\s*\{[^}]*line-height:\s*1\.75;[^}]*letter-spacing:\s*0\.025em;/s);
    expect(css).toMatch(/\.chat-code__pre\s*\{[^}]*line-height:\s*calc\(var\(--spacing\)\s*\*\s*5\)/s);
    expect(css).toMatch(/\.chat-md h1\s*\{[^}]*font-size:\s*var\(--chat-prose-heading-xl\)/s);
    expect(css).toMatch(/\.chat-md h2\s*\{[^}]*font-size:\s*var\(--chat-prose-heading-lg\)/s);
    expect(css).toMatch(/\.chat-md h3\s*\{[^}]*font-size:\s*var\(--chat-prose-heading-base\)/s);
    // ZCode 标题不声明固定行高，继承正文的 1.75。
    expect(css).not.toMatch(/\.chat-md h[1-6]\s*\{[^}]*line-height:/s);
  });
});

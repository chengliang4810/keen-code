import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { MarkdownChat } from "./MarkdownChat";
import { Thinking } from "./Thinking";

describe("MarkdownChat streaming", () => {
  it("does not hide the first real delta behind a soft buffer", () => {
    const html = renderToString(
      <MarkdownChat streaming>第一个真实 reasoning delta</MarkdownChat>,
    );
    const source = readFileSync(
      new URL("./MarkdownChat.tsx", import.meta.url),
      "utf8",
    );

    expect(html).toContain("第一个真实 reasoning delta");
    expect(source).not.toContain("softStreamBuffer");
    expect(source).not.toContain("stepSoftBuffer");
  });

  it("keeps live fences plain and highlights once the message settles", () => {
    const markdown = "```ts\nconst answer: number = 42;\n```";

    const streamingHtml = renderToString(
      <MarkdownChat streaming>{markdown}</MarkdownChat>,
    );
    const settledHtml = renderToString(
      <MarkdownChat>{markdown}</MarkdownChat>,
    );

    expect(streamingHtml).toContain("const answer: number = 42;");
    expect(streamingHtml).not.toContain("hljs-keyword");
    expect(settledHtml).toContain("hljs-keyword");
    expect(settledHtml).toContain("hljs-number");
  });

  it("publishes the latest live reasoning line without mounting a second markdown buffer", () => {
    const html = renderToString(
      <Thinking
        locale="zh"
        thinking
        content={"开始检查\n正在检查实现"}
        processedLabel={(duration) => `已处理 ${duration}`}
      />,
    );

    expect(html).toContain('data-follow-end="true"');
    expect(html).toContain("正在检查实现");
    expect(html).not.toContain("开始检查");
    expect(html).not.toContain("chat-md--streaming");
  });
});

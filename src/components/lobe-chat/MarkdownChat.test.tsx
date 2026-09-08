import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { MarkdownChat } from "./MarkdownChat";
import { MarkdownBody } from "../MarkdownBody";
import { Thinking } from "./Thinking";
import { selectMarkdownTextBlock } from "./markdownTextSelection";

describe("MarkdownChat streaming", () => {
  it("连续三击只选择当前文字块，不包含块级换行或嵌套列表", () => {
    const range = { setStart: vi.fn(), setEnd: vi.fn() } as unknown as Range;
    const selection = {
      removeAllRanges: vi.fn(),
      addRange: vi.fn(),
    } as unknown as Selection;
    const target = {} as HTMLElement;
    const directFirst = {
      nodeType: 3,
      data: "  当前文字",
      parentElement: { closest: () => target },
    } as unknown as Text;
    const nested = {
      nodeType: 3,
      data: "嵌套列表",
      parentElement: { closest: () => ({}) },
    } as unknown as Text;
    const directLast = {
      nodeType: 3,
      data: "结束  ",
      parentElement: { closest: () => target },
    } as unknown as Text;
    const nodes = [directFirst, directLast, nested];
    let nodeIndex = 0;
    Object.assign(target, {
      ownerDocument: {
        getSelection: () => selection,
        createTreeWalker: () => ({
          nextNode: () => nodes[nodeIndex++] ?? null,
        }),
        createRange: () => range,
      },
    });

    expect(selectMarkdownTextBlock(target, 2)).toBe(false);
    expect(selectMarkdownTextBlock(target, 3)).toBe(true);
    expect(range.setStart).toHaveBeenCalledWith(directFirst, 2);
    expect(range.setEnd).toHaveBeenCalledWith(directLast, 2);
    expect(selection.addRange).toHaveBeenCalledWith(range);
  });

  it("keeps a UNC video example as inline code", () => {
    const html = renderToString(
      <MarkdownChat>{"例如 `\\\\server\\share\\video.mp4`"}</MarkdownChat>,
    );

    expect(html).toContain("chat-md__inline-code");
    expect(html).toContain("server\\share\\video.mp4");
    expect(html).not.toContain("video-ui");
  });

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
        statusLabel={(duration, running) =>
          `${running ? "工作中" : "已工作"} ${duration}`
        }
      />,
    );

    expect(html).toContain('data-follow-end="true"');
    expect(html).toContain("正在检查实现");
    expect(html).not.toContain("开始检查");
    expect(html).not.toContain("chat-md--streaming");
  });
});


describe("MarkdownChat URL punctuation", () => {
  it("also fixes resource Markdown without changing Chinese paths or inline code", () => {
    const html = renderToString(
      <MarkdownBody>{"访问 https://example.com/中文?q=测试）：以及 `http://localhost:3000）：`"}</MarkdownBody>,
    );
    expect(html).toContain('href="https://example.com/%E4%B8%AD%E6%96%87?q=%E6%B5%8B%E8%AF%95"');
    expect(html).toContain("</a>）：以及");
    expect(html).toContain("http://localhost:3000）：</code>");
  });

  it.each([false, true])("keeps Chinese sentence endings outside links (streaming=%s)", (streaming) => {
    const html = renderToString(
      <MarkdownChat streaming={streaming}>{"**文件结构**（4 个文件，启动后访问 http://localhost:3000）："}</MarkdownChat>,
    );
    expect(html).toContain("http://localhost:3000");
    expect(html).not.toContain("%EF%BC");
    expect(html).toMatch(/<\/[^>]+>）：/);
  });

  it.each([
    "[地址](https://example.com/文档）)",
    "<https://example.com/文档）>",
    "https://example.com/%EF%BC%89",
  ])("preserves intentional URL punctuation: %s", (source) => {
    const html = renderToString(<MarkdownChat>{source}</MarkdownChat>);
    expect(html).toContain("%EF%BC%89");
  });
});

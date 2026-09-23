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
    expect(settledHtml).toContain("const answer: number = 42;");
    // Shiki 高亮器只在消息 settle 后由客户端 effect 异步上色;流式围栏
    // 完全不触发高亮器(hljs 时代的 class 断言迁移为组件接线断言)。
    const source = readFileSync(
      new URL("./MarkdownChat.tsx", import.meta.url),
      "utf8",
    );
    expect(source).toContain("highlight={!streaming}");
    expect(streamingHtml).not.toContain("chat-code__token");
  });

  it("keeps Markdown horizontal rules in the message flow", () => {
    const html = renderToString(
      <MarkdownChat>{"第一段\n\n---\n\n第二段"}</MarkdownChat>,
    );

    expect(html).toContain("<hr");
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
  it("将对话中的网页链接渲染为无图标的普通超链接", () => {
    const html = renderToString(
      <MarkdownChat>{"[Node.js](https://nodejs.org)"}</MarkdownChat>,
    );

    // streamdown 的 harden 阶段按 WHATWG URL 规范化 href(origin-only 链接
    // 补尾斜杠);这是栈的固有行为,资源面板打开时由服务端自动跳转处理。
    expect(html).toContain('class="chat-md__link"');
    expect(html).toContain('href="https://nodejs.org/"');
    expect(html).toContain(">Node.js</a>");
    expect(html).not.toContain("file-path-link");

    const source = readFileSync(
      new URL("./MarkdownChat.tsx", import.meta.url),
      "utf8",
    );
    expect(source).toContain("event.preventDefault()");
    expect(source).toContain('type: "url"');
    expect(source).toContain("url: hrefStr");
  });

  it("保留 Markdown 网页链接的自定义显示文本", () => {
    const html = renderToString(
      <MarkdownChat>
        {"[插件配置](https://github.com/example/repo/blob/main/plugin.json)"}
      </MarkdownChat>,
    );

    expect(html).toContain(
      'href="https://github.com/example/repo/blob/main/plugin.json"',
    );
    expect(html).toContain(">插件配置</a>");
    expect(html).not.toContain(">plugin.json</a>");
  });

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
    "https://example.com/%EF%BC%89",
  ])("preserves intentional URL punctuation: %s", (source) => {
    const html = renderToString(<MarkdownChat>{source}</MarkdownChat>);
    expect(html).toContain("%EF%BC%89");
  });

  it("尖括号自动链接的 CJK 尾标点由 cjk 插件拆出链接外", () => {
    // 行为差异:@streamdown/cjk 的 autolink 边界拆分(对齐 ZCode 基线)会
    // 把 `<url）>` 形式尾部的 `）` 拆回正文;显式 [文本](url) 仍完整保留。
    const html = renderToString(
      <MarkdownChat>{"<https://example.com/文档）>"}</MarkdownChat>,
    );
    expect(html).toContain("https://example.com/%E6%96%87%E6%A1%A3");
    expect(html).toContain("</a>）");
  });
});

describe("MarkdownChat streamdown 渲染栈", () => {
  it("渲染行内与块级公式(KaTeX)", () => {
    const html = renderToString(
      <MarkdownChat>{"能量守恒 $E=mc^2$。\n\n$$\\int_0^1 x^2\\,dx=\\tfrac{1}{3}$$"}</MarkdownChat>,
    );
    expect(html).toContain("katex");
    expect(html).toContain("E=mc");
  });

  it("美元金额与共享路径不被误判为公式", () => {
    const html = renderToString(
      <MarkdownChat>
        {"价格在 $5-$10 之间;环境变量 $HOME 与 $PATH 不同。\n复制到 D:\\proj\\C$\\out 目录。"}
      </MarkdownChat>,
    );
    expect(html).toContain("$5-$10 之间");
    expect(html).toContain("$HOME");
    expect(html).toContain("C$\\out");
    expect(html).not.toContain("katex");
  });

  it("单波浪线保留原文,双波浪线按 GFM 渲染删除线", () => {
    const html = renderToString(
      <MarkdownChat>{"约 3~5 天完成;~~旧方案~~ 已废弃。"}</MarkdownChat>,
    );
    expect(html).toContain("3~5");
    expect(html).toMatch(/<(del|s)[^>]*>旧方案<\/(del|s)>/);
  });
});
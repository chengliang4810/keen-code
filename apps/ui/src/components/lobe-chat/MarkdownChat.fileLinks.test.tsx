import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FilePathCard } from "@/components/FilePathCard";
import { MarkdownChat } from "./MarkdownChat";
import { StreamdownMarkdown } from "./StreamdownMarkdown";
import { chatUrlTransform } from "./streamdownSetup";

vi.mock("@/components/FilePathCard", () => ({
  FilePathCard: vi.fn(({ displayName }: { displayName?: string }) => (
    <span>{displayName}</span>
  )),
}));

const FILE_LINKS = [
  "D:/projects/keen-code/docs/prompts/current-system-prompt.zh-CN.md",
  "c:/projects/notes.md",
  "Z:/项目/完整中文整理稿.md",
  "D:/项目/有 空格/阅读稿.md",
  "D:\\项目\\阅读稿.md",
  "\\\\server\\share\\阅读稿.md",
  "/home/user/项目/阅读稿.md",
  "/home/user/有 空格/阅读稿.md",
  "D:/projects/name(with-parentheses).md",
];

describe.each([false, true])("完整文件链接 (streaming=%s)", (streaming) => {
  beforeEach(() => {
    vi.mocked(FilePathCard).mockClear();
  });

  it.each(FILE_LINKS)("保留文件地址并接入资源面板：%s", (path) => {
    const onOpenResource = vi.fn();
    const destination = path.replace(/\\/g, "\\\\");
    const html = renderToString(
      <MarkdownChat streaming={streaming} onOpenResource={onOpenResource}>
        {`[完整中文整理稿](<${destination}>)`}
      </MarkdownChat>,
    );
    const props = vi.mocked(FilePathCard).mock.calls.at(-1)?.[0];
    const normalizedPath = path.replace(/\\/g, "/");

    expect(html).toContain("完整中文整理稿");
    expect(html).not.toContain("[blocked]");
    expect(props).toMatchObject({
      path: normalizedPath,
      absolutePath: normalizedPath,
      displayName: "完整中文整理稿",
      kind: "file",
    });

    props?.onOpenInPanel?.({
      type: "file",
      path: normalizedPath,
      title: "完整中文整理稿",
    });
    expect(onOpenResource).toHaveBeenCalledWith({
      type: "file",
      path: normalizedPath,
      title: "完整中文整理稿",
    });
  });

  it("只解码一次文件链接中的百分号转义", () => {
    renderToString(
      <MarkdownChat streaming={streaming}>
        {"[文件](D:/projects/a%2520b%20%E4%B8%AD%E6%96%87.md)"}
      </MarkdownChat>,
    );

    expect(vi.mocked(FilePathCard).mock.calls.at(-1)?.[0].path).toBe(
      "D:/projects/a%20b 中文.md",
    );
  });

  it.each([
    "javascript:alert(1)",
    "JaVaScRiPt:alert(1)",
    "vbscript:msgbox(1)",
    "data:text/html,test",
    "file:///D:/projects/notes.md",
    "d:payload",
  ])("继续拦截危险或非绝对盘符协议：%s", (url) => {
    const html = renderToString(
      <StreamdownMarkdown
        source={`[危险链接](<${url}>)`}
        streaming={streaming}
        components={{
          a: ({ href, children }) => <a href={href}>{children}</a>,
        }}
      />,
    );

    expect(html).not.toMatch(/href="[^"]+"/);
    expect(FilePathCard).not.toHaveBeenCalled();
  });

  it("本地链接仍经过 HTML 属性清洗", () => {
    const html = renderToString(
      <StreamdownMarkdown
        source={'<a href="D:/projects/notes.md" onclick="alert(1)">文件</a><script>alert(1)</script>'}
        streaming={streaming}
        components={{
          a: ({ href, children }) => <a href={href}>{children}</a>,
        }}
      />,
    );

    expect(html).toContain('href="D:/projects/notes.md"');
    expect(html).not.toContain("onclick");
    expect(html).not.toContain("<script");
    expect(html).not.toContain("alert(1)");
  });
});

describe("文件链接 URL 边界", () => {
  it("保留非 URL 百分号路径，不因解码失败中断消息渲染", () => {
    expect(chatUrlTransform("D:/projects/100%done.md")).toBe(
      "D:/projects/100%done.md",
    );
  });

  it.each(["d:payload", "x:alert(1)", "file:///tmp/test.md"])(
    "不再把任意单字母协议当成 Windows 全路径：%s",
    (url) => {
      expect(chatUrlTransform(url)).toBe("");
    },
  );
});

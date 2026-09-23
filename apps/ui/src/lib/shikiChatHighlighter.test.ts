import { describe, expect, it } from "vitest";
import {
  highlightChatCodeLines,
  isPlainChatCodeLanguage,
  type ChatCodeLine,
} from "./shikiChatHighlighter";

function collect(
  code: string,
  language?: string,
): Promise<ChatCodeLine[] | null> {
  return new Promise((resolve) => {
    highlightChatCodeLines(code, language, resolve);
  });
}

const joinLines = (lines: ChatCodeLine[]): string =>
  lines.map((line) => line.map((token) => token.content).join("")).join("\n");

describe("shikiChatHighlighter", () => {
  it("给出逐行 token 并携带双主题 CSS 变量", async () => {
    const code = "const one = 1;\nconst two = 2;";
    const lines = await collect(code, "ts");
    expect(lines).not.toBeNull();
    expect(lines).toHaveLength(2);
    expect(joinLines(lines!)).toBe(code);
    const first = lines![0][0];
    expect(first.content).toBe("const");
    expect(first.style?.["--shiki-light"]).toBeTruthy();
    expect(first.style?.["--shiki-dark"]).toBeTruthy();
  });

  it("跨行 token 在行界拆分,拼接后与原文一致", async () => {
    const code = "/* 注释跨\n第二行 */ const x = 1;";
    const lines = await collect(code, "js");
    expect(lines).not.toBeNull();
    expect(lines!.map((line) => line.map((t) => t.content).join(""))).toEqual([
      "/* 注释跨",
      "第二行 */ const x = 1;",
    ]);
  });

  it("纯文本与未知语言降级为 null", async () => {
    expect(await collect("hi", "text")).toBeNull();
    expect(await collect("hi", "not-a-language-xyz")).toBeNull();
    expect(await collect("hi", undefined)).toBeNull();
  });

  it("重复调用命中缓存,内容保持一致", async () => {
    const code = "fn main() {}";
    const first = await collect(code, "rust");
    const second = await collect(code, "rust");
    expect(second).toEqual(first);
    expect(second).not.toBeNull();
  });

  it("isPlainChatCodeLanguage 识别纯文本拼写", () => {
    expect(isPlainChatCodeLanguage("plaintext")).toBe(true);
    expect(isPlainChatCodeLanguage("language-text")).toBe(true);
    expect(isPlainChatCodeLanguage("language-ts")).toBe(false);
  });
});

import { describe, expect, it } from "vitest";
import { resolveUserMessageClipboardText } from "./userMessageCopy";

describe("resolveUserMessageClipboardText", () => {
  it("ignores WebKit structural line breaks around one user message", () => {
    expect(resolveUserMessageClipboardText("你好啊\n", "你好啊")).toBe(
      "你好啊",
    );
    expect(resolveUserMessageClipboardText("\n你好啊\n\n", "你好啊")).toBe(
      "你好啊",
    );
  });

  it("preserves line breaks selected inside a multiline message", () => {
    expect(
      resolveUserMessageClipboardText(
        "第一行\n第二行\n",
        "第一行\n第二行",
      ),
    ).toBe("第一行\n第二行");
    expect(resolveUserMessageClipboardText("第一行\n\n", "第一行\n")).toBe(
      "第一行\n",
    );
    expect(
      resolveUserMessageClipboardText(
        "第一行\n\n第二行\n",
        "第一行\n\n第二行",
      ),
    ).toBe("第一行\n\n第二行");
  });

  it("ignores WebKit line breaks inserted around skill chips", () => {
    expect(
      resolveUserMessageClipboardText("pdf\n\n正文\n", "pdf\n正文"),
    ).toBe("pdf\n正文");
    expect(
      resolveUserMessageClipboardText(
        "pdf\nwriter\n\n正文\n",
        "pdfwriter\n正文",
      ),
    ).toBe("pdfwriter\n正文");
  });

  it("does not replace cross-message selections", () => {
    expect(
      resolveUserMessageClipboardText("你好啊\n助手回复", "你好啊"),
    ).toBeNull();
  });
});

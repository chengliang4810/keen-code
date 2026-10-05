import { describe, expect, it } from "vitest";
import { applyBundledStyleNonce, createRuntimeStyleElement } from "@/lib/runtimeStyle.js";

function createDocumentStub(nonces: string[], scriptNonces: string[] = []) {
  const createdStyle = { nonce: "" } as HTMLStyleElement;
  const styleElements = nonces.map((nonce) => ({
    nonce,
    getAttribute: () => "",
  }));
  const scriptElements = scriptNonces.map((nonce) => ({ nonce }));
  const document = {
    head: {
      querySelectorAll: (selector: string) => {
        if (selector === "style") return styleElements;
        if (selector === "script") return scriptElements;
        return [...styleElements, ...scriptElements];
      },
    },
    createElement: () => createdStyle,
  } as unknown as Document;
  return { document, createdStyle };
}

describe("runtime style nonce", () => {
  it("复用 bundled style 暴露的 nonce", () => {
    const { document } = createDocumentStub(["", "tauri-style-token"]);
    const style = { nonce: "" } as HTMLStyleElement;

    applyBundledStyleNonce(document, style);

    expect(style.nonce).toBe("tauri-style-token");
  });

  it("创建动态 style 时直接复制 nonce", () => {
    const { document, createdStyle } = createDocumentStub(["tauri-style-token"]);

    expect(createRuntimeStyleElement(document)).toBe(createdStyle);
    expect(createdStyle.nonce).toBe("tauri-style-token");
  });

  it("开发环境没有 nonce 时保持空值", () => {
    const { document } = createDocumentStub([], ["script-only-token"]);
    const style = { nonce: "" } as HTMLStyleElement;

    applyBundledStyleNonce(document, style);

    expect(style.nonce).toBe("");
  });
});

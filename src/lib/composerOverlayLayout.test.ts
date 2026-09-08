import { describe, expect, it } from "vitest";
import { calculateComposerOverlayLayout } from "./composerOverlayLayout";
import { readSource } from "@/test-utils/readCssSource";

describe("底部问答与输入区的独立布局", () => {
  it("没有问答时保持原输入区高度和消息留白", () => {
    expect(calculateComposerOverlayLayout(146, 0)).toEqual({
      composerHeight: 146,
      composerFloatPad: 146,
    });
  });

  it("问答覆盖输入区，消息留白取较大高度", () => {
    expect(calculateComposerOverlayLayout(146, 302)).toEqual({
      composerHeight: 146,
      composerFloatPad: 302,
    });
  });

  it("问答翻页只改变总留白，不改变卡片定位基准", () => {
    const first = calculateComposerOverlayLayout(146, 302);
    const next = calculateComposerOverlayLayout(146, 190);
    expect(next.composerHeight).toBe(first.composerHeight);
    expect(next.composerFloatPad).toBe(190);
  });

  it("输入区高于问答时仍保留完整输入区留白", () => {
    expect(calculateComposerOverlayLayout(220, 190)).toEqual({
      composerHeight: 220,
      composerFloatPad: 220,
    });
  });

  it("小数像素向上取整，不积累上一帧的总高度", () => {
    const measured = calculateComposerOverlayLayout(145.4, 190.2);
    expect(measured).toEqual({ composerHeight: 146, composerFloatPad: 191 });
    expect(calculateComposerOverlayLayout(145.4, 190.2)).toEqual(measured);
    expect(calculateComposerOverlayLayout(145.4, 0).composerFloatPad).toBe(146);
  });

  it("CSS底部对齐并限制问答可滚动高度", () => {
    const css = readSource(new URL("../styles/app-conversation.css", import.meta.url));
    const wrapper = css.match(/\.ask-user-wrap\s*\{([^}]+)\}/)?.[1];
    const card = css.match(/\.ask-user\s*\{([^}]+)\}/)?.[1];
    expect(wrapper).toContain("bottom: 0");
    expect(wrapper).not.toContain("--composer-float-pad");
    expect(card).toContain("max-height: calc(100dvh - 96px)");
    expect(card).toContain("overflow-y: auto");
  });
});

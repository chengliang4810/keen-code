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

  it("问答与输入区相加留白，不再只取较大高度", () => {
    expect(calculateComposerOverlayLayout(146, 302)).toEqual({
      composerHeight: 146,
      composerFloatPad: 448,
    });
  });

  it("问答翻页只改变总留白，不改变卡片定位基准", () => {
    const first = calculateComposerOverlayLayout(146, 302);
    const next = calculateComposerOverlayLayout(146, 190);
    expect(next.composerHeight).toBe(first.composerHeight);
    expect(next.composerFloatPad).toBe(336);
  });

  it("输入区扩展会移动问答，但问答高度不反过来移动输入区", () => {
    expect(calculateComposerOverlayLayout(220, 190)).toEqual({
      composerHeight: 220,
      composerFloatPad: 410,
    });
  });

  it("小数像素向上取整，不积累上一帧的总高度", () => {
    const measured = calculateComposerOverlayLayout(145.4, 190.2);
    expect(measured).toEqual({ composerHeight: 146, composerFloatPad: 336 });
    expect(calculateComposerOverlayLayout(145.4, 190.2)).toEqual(measured);
    expect(calculateComposerOverlayLayout(145.4, 0).composerFloatPad).toBe(146);
  });

  it("CSS仅用输入区高度定位问答并限制可滚动高度", () => {
    const css = readSource(new URL("../styles/app-conversation.css", import.meta.url));
    const wrapper = css.match(/\.ask-user-wrap\s*\{([^}]+)\}/)?.[1];
    const card = css.match(/\.ask-user\s*\{([^}]+)\}/)?.[1];
    expect(wrapper).toContain("bottom: var(--composer-height, 168px)");
    expect(wrapper).not.toContain("--composer-float-pad");
    expect(card).toContain("max-height: calc(100dvh - var(--composer-height, 168px) - 96px)");
    expect(card).toContain("overflow-y: auto");
  });
});

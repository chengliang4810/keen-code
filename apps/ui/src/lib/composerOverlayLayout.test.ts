import { describe, expect, it } from "vitest";
import { normalizeComposerHeight } from "./composerOverlayLayout";
import { readCssSource, readSource } from "@/test-utils/readCssSource";

describe("Composer sticky 停靠布局", () => {
  it("输入区测高只归一化自身高度，不再计算底部留白", () => {
    expect(normalizeComposerHeight(145.4)).toBe(146);
    expect(normalizeComposerHeight(-4)).toBe(0);
    expect(normalizeComposerHeight(Number.NaN)).toBe(0);
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

  it("多行问题下导航组贴右上角，不随文本块垂直居中", () => {
    const css = readSource(new URL("../styles/app-conversation.css", import.meta.url));
    const header = css.match(/\.ask-user__header\s*\{([^}]+)\}/)?.[1];
    expect(header).toContain("align-items: flex-start");
  });

  it("会话 Composer 挂载在滚动视口内的 sticky dock，移除绝对覆盖和固定 pad", () => {
    const css = [
      readCssSource(new URL("../styles/app.css", import.meta.url)),
      readSource(new URL("../components/lobe-chat/lobe-chat.css", import.meta.url)),
    ].join("\n");
    const dock = css.match(/\.lobe-chat__composer-dock\s*\{([^}]*)\}/)?.[1];
    const sticky = css.match(/\.composer-wrap--sticky\s*\{([^}]*)\}/)?.[1];
    expect(dock).toContain("position: sticky");
    expect(dock).toContain("bottom: 0");
    expect(sticky).toContain("pointer-events: none");
    expect(sticky).not.toContain("position: absolute");
    expect(sticky).not.toContain("linear-gradient");
    expect(css).not.toContain("composer-float-pad");
    expect(css).toContain(".lobe-chat__scroll-content");
  });

  it("桌面 sticky dock 的内边距包含在 56/72rem 最大宽度中", () => {
    const css = readSource(new URL("../styles/app-conversation.css", import.meta.url));
    const sticky = css.match(/\.composer-wrap--sticky\s*\{([^}]*)\}/)?.[1] ?? "";

    expect(sticky).toContain("padding: 0 16px 16px;");
    expect(css).toMatch(
      /\.composer-stack\s*\{[^}]*max-width:\s*min\(100%,\s*calc\(72rem - 32px\)\);/s,
    );
    expect(css).toMatch(
      /@container conversation-stage \(min-width: 864px\)[\s\S]*?\.composer-stack\s*\{[^}]*max-width:\s*min\(calc\(100% - 6rem\),\s*calc\(56rem - 32px\)\);/s,
    );
    expect(css).toMatch(
      /@container conversation-stage \(min-width: 1280px\)[\s\S]*?\.composer-stack\s*\{[^}]*max-width:\s*min\(calc\(100% - 24rem\),\s*calc\(72rem - 32px\)\);/s,
    );
  });
});

describe("Composer 输入层叠契约", () => {
  it("所有输入框规则都采用 Lexical 的 40/160 高度并保留双侧工具簇", () => {
    const css = readCssSource(new URL("../styles/app.css", import.meta.url));
    const inputRules = [
      ...css.matchAll(/(?:^|\n)(?:div\.)?\.?composer__input\s*\{([^}]*)\}/g),
    ]
      .map((match) => match[1])
      .filter((rule) => /min-height:|max-height:/.test(rule));

    expect(inputRules.length).toBeGreaterThanOrEqual(2);
    for (const rule of inputRules) {
      expect(rule).toContain("min-height: 40px");
      expect(rule).toContain("max-height: 160px");
    }
    expect(css).not.toMatch(
      /div\.composer__input\s*\{[^}]*min-height: 28px/s,
    );
    expect(css).not.toMatch(
      /div\.composer__input\s*\{[^}]*max-height: 336px/s,
    );
    expect(css).not.toContain(".composer-wrap--welcome div.composer__input");
    expect(css).toContain(".composer__leading-actions");
    expect(css).toContain(".composer__trailing-actions");
  });
});

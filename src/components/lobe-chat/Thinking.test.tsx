import React from "react";
import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  formatProcessingDuration,
  getReasoningBottomDistance,
  getReasoningScrollMaskStyle,
  getReasoningSummaryMaskStyle,
  isReasoningScrollAtBottom,
  REASONING_CONTENT_UNLOAD_DELAY_MS,
  reasoningSummary,
  resolveReasoningStreamingSummary,
  resolveReasoningScrollMaskState,
  syncReasoningSummaryScroll,
  Thinking,
} from "./Thinking";

describe("Thinking processing duration", () => {
  it("只在打开的流式思考上启动计时器，并在关闭动画缺失时兜底卸载正文", () => {
    const source = readFileSync(new URL("./Thinking.tsx", import.meta.url), "utf8");

    expect(REASONING_CONTENT_UNLOAD_DELAY_MS).toBe(300);
    expect(source).toContain("if (!open) return;");
    expect(source).toMatch(
      /const unloadTimer = window\.setTimeout\([\s\S]*?REASONING_CONTENT_UNLOAD_DELAY_MS/,
    );
    expect(source).toContain('event.propertyName === "height"');
    expect(source).toContain("setShouldRenderContent(false)");
  });

  it("按中文分秒格式展示处理时间", () => {
    expect(formatProcessingDuration(0, "zh")).toBe("1秒");
    expect(formatProcessingDuration(999, "zh")).toBe("1秒");
    expect(formatProcessingDuration(-1_000, "zh")).toBe("1秒");
    expect(formatProcessingDuration(Number.NaN, "zh")).toBe("1秒");
    expect(formatProcessingDuration(122_900, "zh")).toBe("2分钟 2秒");
    expect(formatProcessingDuration(122_900, "zh-TW")).toBe("2分鐘 2秒");
  });

  it("按中文展示小时与天单位的处理时间", () => {
    expect(formatProcessingDuration(3_660_000, "zh")).toBe("1小时 1分钟");
    expect(formatProcessingDuration(30_000_000, "zh")).toBe("8小时 20分钟");
    expect(formatProcessingDuration(90_000_000, "zh")).toBe("1天 1小时");
    expect(formatProcessingDuration(3_660_000, "zh-TW")).toBe("1小時 1分鐘");
    expect(formatProcessingDuration(90_000_000, "zh-TW")).toBe("1天 1小時");
  });

  it("按英文紧凑格式展示处理时间", () => {
    expect(formatProcessingDuration(9_800, "en")).toBe("9s");
    expect(formatProcessingDuration(122_900, "en")).toBe("2m 2s");
    expect(formatProcessingDuration(3_660_000, "en")).toBe("1h 1m");
    expect(formatProcessingDuration(30_000_000, "en")).toBe("8h 20m");
    expect(formatProcessingDuration(90_000_000, "en")).toBe("1d 1h");
  });

  it("区分工作中与已完成的思考摘要", () => {
    const statusLabel = (duration: string, running: boolean) =>
      `${running ? "工作中" : "已工作"} ${duration}`;
    const liveHtml = renderToString(
      React.createElement(Thinking, {
        thinking: true,
        durationMs: 1_000,
        statusLabel,
        locale: "zh",
      }),
    );
    const completedHtml = renderToString(
      React.createElement(Thinking, {
        content: "已经完成分析",
        thinking: false,
        durationMs: 11_000,
        statusLabel,
        locale: "zh",
      }),
    );
    const css = readFileSync(new URL("./lobe-chat.css", import.meta.url), "utf8");

    expect(liveHtml).toContain("工作中 1秒");
    expect(completedHtml).toContain("思考过程");
    expect(completedHtml).toContain("持续了 11秒");
    expect(liveHtml).toContain("animated-gradient-text");
    expect(completedHtml).not.toContain("animated-gradient-text");
    expect(css).toMatch(/\.lobe-chat-thinking__body\s*\{[^}]*border-left:/s);
    expect(css).toMatch(
      /\.lobe-chat-thinking__icon\s*\{[^}]*transform:\s*none/s,
    );
  });

  it("Brain 图标使用 16x16 leading slot，并保留 Thinking 的 DOM 语义类", () => {
    const source = readFileSync(new URL("./Thinking.tsx", import.meta.url), "utf8");
    const css = readFileSync(new URL("./lobe-chat.css", import.meta.url), "utf8");

    expect(source).toMatch(
      /<span className="lobe-chat-thinking__leading"[^>]*>[\s\S]*?<IconBrain size=\{16\} className="lobe-chat-thinking__icon" \/>/,
    );
    expect(css).toMatch(
      /\.lobe-chat-thinking__leading\s*\{[^}]*width:\s*16px;[^}]*height:\s*16px;/s,
    );
    expect(css).toMatch(
      /\.lobe-chat-thinking__icon\s*\{[^}]*color:\s*var\(--chat-text-3\);[^}]*transform:\s*none/s,
    );
  });

  it("运行中展示完整末行，完成后折叠状态不显示摘要，且均默认折叠", () => {
    const firstLine =
      "Inspect the session without slicing this completed summary";
    const latestLine =
      "Newest reasoning tokens keep arriving without character clipping";
    const liveHtml = renderToString(
      React.createElement(Thinking, {
        content: `${firstLine}\n${latestLine}\n`,
        thinking: true,
        durationMs: 4_000,
        statusLabel: (duration: string, running: boolean) =>
          `${running ? "工作中" : "已工作"} ${duration}`,
        locale: "zh",
      }),
    );
    const completedHtml = renderToString(
      React.createElement(Thinking, {
        content: `${firstLine}\n${latestLine}`,
        thinking: false,
        durationMs: 4_000,
        statusLabel: (duration: string, running: boolean) =>
          `${running ? "工作中" : "已工作"} ${duration}`,
        locale: "zh",
      }),
    );

    expect(liveHtml).toContain('data-variant="think"');
    expect(liveHtml).toContain('data-state="running"');
    expect(liveHtml).toContain('aria-expanded="false"');
    expect(liveHtml).toContain("思考");
    expect(liveHtml).toContain(latestLine);
    expect(liveHtml).not.toContain(firstLine);
    expect(liveHtml).toContain('data-follow-end="true"');
    expect(completedHtml).toContain('aria-expanded="false"');
    expect(completedHtml).not.toContain(firstLine);
    expect(completedHtml).not.toContain(latestLine);
    expect(completedHtml).not.toContain("data-follow-end");
  });

  it("摘要不做字符截取，并按真实宽度跟随流式文本末端", () => {
    const longLine = "长".repeat(160);
    expect(reasoningSummary(`首行\n${longLine}\n`, true)).toBe(longLine);
    expect(reasoningSummary(`${longLine}\n末行`, false)).toBe(longLine);
    expect(resolveReasoningStreamingSummary(" 首行\r\n\t\r\n  最新一行  \r\n")).toBe(
      "最新一行",
    );
    expect(resolveReasoningStreamingSummary("\r\n  \r\n")).toBe("");

    const element = {
      clientWidth: 100,
      scrollLeft: 0,
      scrollWidth: 360,
    };
    syncReasoningSummaryScroll(element, true);
    expect(element.scrollLeft).toBe(260);
    syncReasoningSummaryScroll(element, false);
    expect(element.scrollLeft).toBe(0);
  });

  it("正文离底后暂停吸底，并为上下隐藏内容提供 mask 状态", () => {
    expect(getReasoningBottomDistance({ clientHeight: 240, scrollHeight: 640, scrollTop: 398 })).toBe(2);
    expect(isReasoningScrollAtBottom({ clientHeight: 240, scrollHeight: 640, scrollTop: 398 })).toBe(true);
    expect(isReasoningScrollAtBottom({ clientHeight: 240, scrollHeight: 640, scrollTop: 200 })).toBe(false);
    expect(resolveReasoningScrollMaskState({ clientHeight: 240, scrollHeight: 640, scrollTop: 0 })).toBe("bottom");
    expect(resolveReasoningScrollMaskState({ clientHeight: 240, scrollHeight: 640, scrollTop: 200 })).toBe("both");
    expect(resolveReasoningScrollMaskState({ clientHeight: 240, scrollHeight: 640, scrollTop: 400 })).toBe("top");
    expect(getReasoningScrollMaskStyle("both")).toMatchObject({
      maskImage: expect.stringContaining("transparent"),
      WebkitMaskImage: expect.stringContaining("transparent"),
    });
    expect(getReasoningScrollMaskStyle("none")).toBeUndefined();
  });

  it("流式摘要溢出时使用左右渐隐 mask", () => {
    expect(getReasoningSummaryMaskStyle(false)).toBeUndefined();
    expect(getReasoningSummaryMaskStyle(true)).toMatchObject({
      maskImage: expect.stringContaining("to right"),
      WebkitMaskImage: expect.stringContaining("to right"),
    });
  });

  it("运行扫光尊重 reduced-motion，展开的长正文限制高度并独立滚动", () => {
    const css = readFileSync(
      new URL("./lobe-chat.css", import.meta.url),
      "utf8",
    );

    expect(css).toMatch(
      /\.animated-gradient-text\s*\{[\s\S]*?background-clip:\s*text;[\s\S]*?animation:\s*gradient-flow 4s linear infinite;/,
    );
    expect(css).toMatch(/@keyframes gradient-flow\s*\{[\s\S]*?background-position:/);
    expect(css).toMatch(
      /@media \(prefers-reduced-motion: reduce\)\s*\{[\s\S]*?\.animated-gradient-text\s*\{[^}]*animation:\s*none;/,
    );
    expect(css).toMatch(
      /\.lobe-chat-thinking__content-inner\s*\{[^}]*padding-top:\s*12px;/s,
    );
    expect(css).toMatch(
      /\.lobe-chat-thinking__body\s*\{[^}]*max-height:\s*240px;/s,
    );
    expect(css).toMatch(
      /\.lobe-chat-thinking__body\s*\{[^}]*overflow:\s*auto;/s,
    );
    expect(css).toMatch(
      /\.lobe-chat-thinking__body\s*\{[^}]*overscroll-behavior:\s*contain;/s,
    );
  });
});

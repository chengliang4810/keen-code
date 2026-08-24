import React from "react";
import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  formatProcessingDuration,
  reasoningSummary,
  syncReasoningSummaryScroll,
  Thinking,
} from "./Thinking";

describe("Thinking processing duration", () => {
  it("按中文分秒格式展示处理时间", () => {
    expect(formatProcessingDuration(0, "zh")).toBe("1秒");
    expect(formatProcessingDuration(999, "zh")).toBe("1秒");
    expect(formatProcessingDuration(-1_000, "zh")).toBe("1秒");
    expect(formatProcessingDuration(Number.NaN, "zh")).toBe("1秒");
    expect(formatProcessingDuration(122_900, "zh")).toBe("2分钟 2秒");
    expect(formatProcessingDuration(122_900, "zh-TW")).toBe("2分鐘 2秒");
  });

  it("按英文紧凑格式展示处理时间", () => {
    expect(formatProcessingDuration(9_800, "en")).toBe("9s");
    expect(formatProcessingDuration(122_900, "en")).toBe("2m 2s");
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
    expect(css).toMatch(/\.lobe-chat-thinking__body\s*\{[^}]*border-left:/s);
    expect(css).toMatch(
      /\.lobe-chat-thinking__icon\s*\{[^}]*transform:\s*translateX\(-3px\)/s,
    );
  });

  it("运行中展示完整末行，完成后恢复完整首行，且均默认折叠", () => {
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
    expect(completedHtml).toContain(firstLine);
    expect(completedHtml).not.toContain(latestLine);
    expect(completedHtml).not.toContain("data-follow-end");
  });

  it("摘要不做字符截取，并按真实宽度跟随流式文本末端", () => {
    const longLine = "长".repeat(160);
    expect(reasoningSummary(`首行\n${longLine}\n`, true)).toBe(longLine);
    expect(reasoningSummary(`${longLine}\n末行`, false)).toBe(longLine);

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

  it("运行扫光尊重 reduced-motion，展开正文不再使用内部滚动区", () => {
    const css = readFileSync(
      new URL("./lobe-chat.css", import.meta.url),
      "utf8",
    );

    expect(css).toMatch(
      /@media \(prefers-reduced-motion: reduce\)\s*\{[\s\S]*?\.lobe-chat-thinking__trigger::after\s*\{[^}]*animation:\s*none;/,
    );
    expect(css).not.toMatch(
      /\.lobe-chat-thinking__body\s*\{[^}]*max-height:/s,
    );
    expect(css).not.toMatch(
      /\.lobe-chat-thinking__body\s*\{[^}]*overflow-y:\s*auto/s,
    );
  });
});

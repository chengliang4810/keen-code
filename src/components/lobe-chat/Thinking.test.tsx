import React from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { formatProcessingDuration, Thinking } from "./Thinking";

describe("Thinking processing duration", () => {
  it("按中文分秒格式展示处理时间", () => {
    expect(formatProcessingDuration(0, "zh")).toBe("1秒");
    expect(formatProcessingDuration(999, "zh")).toBe("1秒");
    expect(formatProcessingDuration(-1_000, "zh")).toBe("1秒");
    expect(formatProcessingDuration(Number.NaN, "zh")).toBe("1秒");
    expect(formatProcessingDuration(122_900, "zh")).toBe("2分2秒");
  });

  it("按英文紧凑格式展示处理时间", () => {
    expect(formatProcessingDuration(9_800, "en")).toBe("9s");
    expect(formatProcessingDuration(122_900, "en")).toBe("2m 2s");
  });

  it("处理中展开思考正文，完成后默认折叠", () => {
    const liveHtml = renderToString(
      React.createElement(Thinking, {
        content: "正在分析",
        thinking: true,
        durationMs: 4_000,
        processedLabel: (duration: string) => `已处理 ${duration}`,
        locale: "zh",
      }),
    );
    const completedHtml = renderToString(
      React.createElement(Thinking, {
        content: "分析完成",
        thinking: false,
        durationMs: 4_000,
        processedLabel: (duration: string) => `已处理 ${duration}`,
        locale: "zh",
      }),
    );

    expect(liveHtml).toContain('aria-expanded="true"');
    expect(liveHtml).toContain("正在分析");
    expect(completedHtml).toContain('aria-expanded="false"');
    expect(completedHtml).not.toContain("分析完成");
  });
});

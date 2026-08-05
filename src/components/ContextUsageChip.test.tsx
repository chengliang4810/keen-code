import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ContextUsageChip } from "./ContextUsageChip";

describe("ContextUsageChip", () => {
  it("使用圆环展示占用，并把详细用量保留在悬浮提示与无障碍名称中", () => {
    const html = renderToString(
      <ContextUsageChip
        display={{
          tokens: 32_000,
          source: "known",
          label: "32k / 128k",
          contextWindow: 128_000,
          percentage: 25,
        }}
        labels={{ aria: "上下文用量" }}
      />,
    );

    expect(html).toContain("context-ring");
    expect(html).toContain('width="16"');
    expect(html).toContain("上下文用量: 32k / 128k");
    expect(html).not.toContain("chip__label--nums");
  });
});

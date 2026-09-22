import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  createStructuredDiffPreviewPlan,
  PLAIN_DIFF_PREVIEW_MAX_BYTES,
  PLAIN_DIFF_PREVIEW_MAX_LINES,
  STRUCTURED_DIFF_MAX_BYTES,
  STRUCTURED_DIFF_MAX_LINES,
} from "./StructuredDiffPreview";

describe("StructuredDiffPreview 渲染边界", () => {
  it("昂贵 Pierre 子树按 patch、文案和显式主题属性记忆化", () => {
    const source = readFileSync(
      fileURLToPath(new URL("./StructuredDiffPreview.tsx", import.meta.url)),
      "utf8",
    );

    expect(source).toContain("const PierreDiff = memo");
    expect(source).toContain('themeType: "light" | "dark"');
    expect(source).toContain("themeType={themeType}");
    expect(source).toContain("[themeType]");
  });

  it("在 256 KiB 边界内使用 Pierre，超过后限制原生预览字节数", () => {
    const atLimit = "a".repeat(STRUCTURED_DIFF_MAX_BYTES);
    expect(createStructuredDiffPreviewPlan(atLimit)).toMatchObject({
      usePierre: true,
      truncated: false,
    });

    const overLimit = `${atLimit}a`;
    const plan = createStructuredDiffPreviewPlan(overLimit);
    expect(plan.usePierre).toBe(false);
    expect(plan.truncated).toBe(true);
    expect(new TextEncoder().encode(plan.plainText).byteLength).toBe(
      PLAIN_DIFF_PREVIEW_MAX_BYTES,
    );
  });

  it("按 UTF-8 字节而不是 JavaScript 字符长度判断多字节差异", () => {
    const atLimit = "界".repeat(Math.floor(STRUCTURED_DIFF_MAX_BYTES / 3));
    expect(createStructuredDiffPreviewPlan(atLimit).usePierre).toBe(true);

    const plan = createStructuredDiffPreviewPlan(`${atLimit}界`);
    expect(plan.usePierre).toBe(false);
    expect(
      new TextEncoder().encode(plan.plainText).byteLength,
    ).toBeLessThanOrEqual(PLAIN_DIFF_PREVIEW_MAX_BYTES);
  });

  it("最多结构化渲染 4000 行，降级文本最多保留 2000 行", () => {
    const atLimit = Array.from(
      { length: STRUCTURED_DIFF_MAX_LINES },
      (_, index) => `+line ${index}`,
    ).join("\n");
    expect(createStructuredDiffPreviewPlan(atLimit).usePierre).toBe(true);

    const overLimit = `${atLimit}\n+one more line`;
    const plan = createStructuredDiffPreviewPlan(overLimit);
    expect(plan.usePierre).toBe(false);
    expect(plan.truncated).toBe(true);
    expect(plan.plainText.split("\n")).toHaveLength(
      PLAIN_DIFF_PREVIEW_MAX_LINES,
    );
  });
});

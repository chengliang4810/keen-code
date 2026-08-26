import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ComposerReasoningMenu } from "./ComposerReasoningMenu";

const labels = {
  reasoning: "推理强度",
  reasoningUnsupported: "不支持",
  ultra: "Ultra",
  ultraDescription: "主动委派复杂工作",
  effortNone: "关闭",
  effortMinimal: "最小",
  effortHigh: "高",
  effortMedium: "中",
  effortLow: "低",
  effortXHigh: "极高",
  effortMax: "最大",
};

describe("ComposerReasoningMenu", () => {
  it("独立触发器显示当前模型支持的本地化推理强度", () => {
    const html = renderToString(
      <ComposerReasoningMenu
        open={false}
        onOpenChange={() => {}}
        model={{
          id: "gpt-5",
          label: "GPT-5",
          reasoningSupported: true,
          reasoningEfforts: [{ id: "low" }, { id: "medium" }, { id: "high" }],
        }}
        effort="medium"
        ultra={false}
        labels={labels}
        onEffort={() => {}}
        onUltra={() => {}}
      />,
    );

    expect(html).toContain('aria-label="推理强度: 中"');
    expect(html).toContain(">中<");
  });

  it("面板使用单值离散 Slider，Ultra 保持独立 Switch", () => {
    const source = readFileSync(
      new URL("./ComposerReasoningMenu.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("value={[Math.max(0, effortIndex)]}");
    expect(source).toContain("step={1}");
    expect(source).toContain("<Switch");
    expect(source).toContain("checked={ultra}");
  });
});

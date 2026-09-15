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
  it("未选择模型时不显示思考强度入口，即使面板状态为打开", () => {
    for (const open of [false, true]) {
      expect(renderToString(
        <ComposerReasoningMenu
          open={open}
          onOpenChange={() => {}}
          effort="medium"
          ultra={false}
          labels={labels}
          onEffort={() => {}}
          onUltra={() => {}}
        />,
      )).toBe("");
    }
  });

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

  it("面板使用 EffortSlider 胶囊滑块，Ultra 是左上角亮起按钮", () => {
    const source = readFileSync(
      new URL("./ComposerReasoningMenu.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("<EffortSlider");
    expect(source).not.toContain("<Slider ");
    expect(source).toContain("fast={ultra}");
    expect(source).toContain('className={`effort-panel__fast ${ultra ? "is-on" : ""}`}');
    expect(source).toContain("aria-pressed={ultra}");
    expect(source).toContain("onUltra(!ultra)");
    // 描述行与 Switch 已移除。
    expect(source).not.toContain("<Switch");
    expect(source).not.toContain("ultraDescription");
    expect(source.match(/<DropdownMenuSeparator/g)).toBeNull();
  });
});

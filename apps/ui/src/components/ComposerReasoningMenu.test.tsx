import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ComposerReasoningMenu, effortTitleClass } from "./ComposerReasoningMenu";

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
  it("Ultra 不控制思考强度动效，标题只随最高档变化", () => {
    expect(effortTitleClass(false)).toBe("effort-title effort-title--fast");
    expect(effortTitleClass(true)).toBe("effort-title effort-title--fusion");
  });

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

  it("面板使用 Appica Slider，Ultra 使用 Appica Button 状态变体", () => {
    const source = readFileSync(
      new URL("./ComposerReasoningMenu.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("<EffortSlider");
    expect(source).toContain('<Tip label={`${labels.reasoning}: ${triggerLabel}`}>');
    expect(source).not.toContain("disabled={open}");
    expect(source).not.toContain("<Slider ");
    expect(source).not.toContain("fast={ultra}");
    // Ultra 开启态走柔和品牌色圆底（CSS），不再切主色实心变体。
    expect(source).not.toContain('variant={ultra ? "primary"');
    expect(source).toContain('size="icon-md"');
    expect(source).toContain("aria-pressed={ultra}");
    expect(source).toContain("onUltra(!ultra)");
    // 描述行与 Switch 已移除。
    expect(source).not.toContain("<Switch");
    expect(source).not.toContain("ultraDescription");
    expect(source.match(/<DropdownMenuSeparator/g)).toBeNull();
  });

  it("Ultra 开启时触发器带闪电标记并把状态并入可读名称，关闭时不出现", () => {
    const render = (ultra: boolean) => renderToString(
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
        ultra={ultra}
        labels={labels}
        onEffort={() => {}}
        onUltra={() => {}}
      />,
    );
    const on = render(true);
    expect(on).toContain("cmm__ultra-mark");
    expect(on).toContain('aria-label="推理强度: 中 · Ultra"');
    const off = render(false);
    expect(off).not.toContain("cmm__ultra-mark");
    expect(off).toContain('aria-label="推理强度: 中"');
  });
});

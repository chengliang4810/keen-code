import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import {
  ComposerPlanModeChip,
  ComposerPlanModeHint,
} from "./ComposerPlanModeChip";

describe("ComposerPlanModeChip", () => {
  it("默认态渲染关闭的开关按钮", () => {
    const html = renderToString(
      <ComposerPlanModeChip
        locale="zh"
        active={false}
        onToggle={vi.fn()}
      />,
    );
    expect(html).toContain("composer-plan-chip");
    expect(html).not.toContain("composer-plan-chip--active");
    expect(html).toContain('aria-pressed="false"');
    expect(html).toContain("计划");
    expect(html).toContain("切换计划模式");
    expect(html).not.toContain("composer-plan-chip__icon--clear");
  });

  it("激活态带 active 修饰类、关闭文案与悬停关闭图标", () => {
    const html = renderToString(
      <ComposerPlanModeChip locale="zh" active={true} onToggle={vi.fn()} />,
    );
    expect(html).toContain("composer-plan-chip--active");
    expect(html).toContain('aria-pressed="true"');
    expect(html).toContain("关闭计划模式");
    expect(html).toContain("composer-plan-chip__icon--plan");
    expect(html).toContain("composer-plan-chip__icon--clear");
  });

  it("英文文案与禁用态", () => {
    const html = renderToString(
      <ComposerPlanModeChip
        locale="en"
        active={false}
        onToggle={vi.fn()}
        disabled={true}
      />,
    );
    expect(html).toContain("Plan");
    expect(html).toContain("Toggle plan mode");
    expect(html).toContain('disabled=""');
  });
});

describe("ComposerPlanModeHint", () => {
  it("未激活时不渲染任何节点", () => {
    expect(
      renderToString(<ComposerPlanModeHint locale="zh" active={false} />),
    ).toBe("");
  });

  it("激活时展示只读调研提示", () => {
    const html = renderToString(
      <ComposerPlanModeHint locale="zh" active={true} />,
    );
    expect(html).toContain("composer-plan-hint");
    expect(html).toContain("计划模式");
    expect(html).toContain("不会修改文件");
  });
});

import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import {
  ComposerGoalChip,
  ComposerGoalProgress,
  formatGoalElapsed,
} from "./ComposerGoalProgress";

describe("ComposerGoalProgress", () => {
  it("展示进行中的目标、耗时与管理操作", () => {
    const html = renderToString(
      <ComposerGoalProgress
        locale="zh"
        goal={{
          revision: 0,
          goal: {
            id: "goal-1",
            title: "测试目标模式 UI",
            objective: "测试目标模式 UI：保持目标处于进行中",
            scope: "project",
            status: "active",
            createdAtMs: 0,
            updatedAtMs: 0,
            tokensUsed: 0,
            timeUsedSeconds: 15,
          },
        }}
        onEdit={vi.fn()}
        onClear={vi.fn()}
      />,
    );

    expect(html).toContain("进行中的目标");
    expect(html).toContain("测试目标模式 UI：保持目标处于进行中");
    expect(html).toContain("15s");
    expect(html).toContain("编辑目标");
    expect(html).toContain("清除目标");
  });

  it("目标模式标签与紧凑耗时按当前规则展示", () => {
    expect(formatGoalElapsed(15)).toBe("15s");
    expect(formatGoalElapsed(125)).toBe("2m");
    expect(renderToString(<ComposerGoalChip locale="zh" onClear={vi.fn()} />))
      .toContain("目标");
  });
});

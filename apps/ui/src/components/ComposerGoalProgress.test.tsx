import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import {
  ComposerGoalChip,
  ComposerGoalProgress,
  formatGoalElapsed,
  goalElapsedSeconds,
} from "./ComposerGoalProgress";

describe("ComposerGoalProgress", () => {
  it("完成后不在输入框保留目标描述和操作按钮", () => {
    const html = renderToString(<ComposerGoalProgress locale="zh"
      goal={{ revision: 2, goal: {
        id: "done", title: "已交付", objective: "已交付的目标描述",
        scope: "session", status: "completed", createdAtMs: 0,
        updatedAtMs: 1, tokensUsed: 0, timeUsedSeconds: 0,
      } }} onEdit={vi.fn()} onClear={vi.fn()} onPause={vi.fn()} onResume={vi.fn()} />);
    expect(html).toBe("");
  });
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
            scope: "session",
            status: "active",
            createdAtMs: 0,
            updatedAtMs: 0,
            tokensUsed: 0,
            timeUsedSeconds: 15,
          },
        }}
        onEdit={vi.fn()}
        onClear={vi.fn()}
        onPause={vi.fn()}
        onResume={vi.fn()}
      />,
    );

    expect(html).toContain("待继续的目标");
    expect(html).toContain("测试目标模式 UI：保持目标处于进行中");
    expect(html).toContain("composer-goal__elapsed");
    expect(html).toContain("15s");
    expect(html).toContain("编辑目标");
    expect(html).toContain("清除目标");
    expect(html).toContain("暂停目标");
  });

  it("目标模式标签与紧凑耗时按当前规则展示", () => {
    expect(formatGoalElapsed(15)).toBe("15s");
    expect(formatGoalElapsed(125)).toBe("2m");
    expect(renderToString(<ComposerGoalChip locale="zh" onClear={vi.fn()} />))
      .toContain("目标");
  });

  it("目标栏只保留状态与操作，不把迭代列表挤在输入框上方", () => {
    const html = renderToString(<ComposerGoalProgress
      locale="zh"
      goal={{ revision: 2, goal: {
        id: "goal-1", title: "目标", objective: "交付", scope: "session",
        status: "active", createdAtMs: 0, updatedAtMs: 0,
        tokensUsed: 0, timeUsedSeconds: 0,
      } }}
      onEdit={vi.fn()} onClear={vi.fn()} onPause={vi.fn()} onResume={vi.fn()}
    />);
    expect(html).not.toContain("目标迭代");
    expect(html).not.toContain("第 1 次迭代");
  });

  it("目标创建后的空闲时间不计入运行耗时", () => {
    expect(goalElapsedSeconds({
      id: "goal-1",
      title: "长时间目标",
      objective: "长时间目标",
      scope: "session",
      status: "active",
      createdAtMs: 1_000,
      updatedAtMs: 1_000,
      tokensUsed: 0,
      timeUsedSeconds: 0,
    })).toBe(0);
  });

  it("持久化耗时更大或目标未运行时不倒退", () => {
    const goal = {
      id: "goal-1",
      title: "目标",
      objective: "目标",
      scope: "session" as const,
      status: "active" as const,
      createdAtMs: 1_000,
      updatedAtMs: 1_000,
      tokensUsed: 0,
      timeUsedSeconds: 120,
    };
    expect(goalElapsedSeconds(goal)).toBe(120);
    expect(goalElapsedSeconds(goal, 240)).toBe(240);
  });
});

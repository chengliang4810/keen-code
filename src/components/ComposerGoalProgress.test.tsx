import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { readCssSource } from "../test-utils/readCssSource";
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
            created_at: "",
            updated_at: "",
            tokens_used: 0,
            time_used_seconds: 15,
          },
        }}
        onEdit={vi.fn()}
        onComplete={vi.fn()}
        onBlock={vi.fn()}
        onClear={vi.fn()}
      />,
    );

    expect(html).toContain("进行中的目标");
    expect(html).toContain("测试目标模式 UI：保持目标处于进行中");
    expect(html).toContain("15s");
    expect(html).toContain("编辑目标");
    expect(html).toContain("标记完成");
    expect(html).toContain("标记阻塞");
    expect(html).toContain("清除目标");
    const liveRegion = html.match(
      /<span class="sr-only"[^>]*role="status"[^>]*>(.*?)<\/span>/,
    );
    expect(liveRegion?.[1]).toBe("进行中的目标");
    expect(liveRegion?.[1]).not.toContain("15s");
  });

  it("仅 active Goal 展示完成与阻塞入口，终态仍保留编辑和清除", () => {
    const goal = {
      revision: 3,
      goal: {
        id: "goal-1",
        title: "终态展示",
        objective: "终态展示",
        scope: "project" as const,
        status: "completed" as const,
        created_at: "",
        updated_at: "",
        tokens_used: 0,
        time_used_seconds: 0,
      },
    };
    const terminalHtml = renderToString(
      <ComposerGoalProgress
        locale="zh"
        goal={goal}
        onEdit={vi.fn()}
        onComplete={vi.fn()}
        onBlock={vi.fn()}
        onClear={vi.fn()}
      />,
    );

    expect(terminalHtml).not.toContain("标记完成");
    expect(terminalHtml).not.toContain("标记阻塞");
    expect(terminalHtml).toContain("编辑目标");
    expect(terminalHtml).toContain("清除目标");
  });

  it("状态转换 pending 时禁用完成与阻塞入口并标记忙碌", () => {
    const html = renderToString(
      <ComposerGoalProgress
        locale="zh"
        goal={{
          revision: 0,
          goal: {
            id: "goal-1",
            title: "等待状态转换",
            objective: "等待状态转换",
            scope: "project",
            status: "active",
            created_at: "",
            updated_at: "",
            tokens_used: 0,
            time_used_seconds: 0,
          },
        }}
        onEdit={vi.fn()}
        onComplete={vi.fn()}
        onBlock={vi.fn()}
        onClear={vi.fn()}
        goalTransitionPending
      />,
    );

    expect(html.match(/aria-busy="true"/g)).toHaveLength(2);
    expect(html.match(/disabled=""/g)).toHaveLength(4);
  });

  it("目标模式标签与紧凑耗时按当前规则展示", () => {
    expect(formatGoalElapsed(15)).toBe("15s");
    expect(formatGoalElapsed(125)).toBe("2m");
    expect(renderToString(<ComposerGoalChip locale="zh" onClear={vi.fn()} />))
      .toContain("目标");
  });

  it("窄屏保留完成与阻塞操作，并让摘要列负责收缩", () => {
    const css = readCssSource(new URL("../styles/app.css", import.meta.url));

    expect(css).toContain(
      "grid-template-columns: 18px minmax(0, 1fr) auto repeat(4, 28px);",
    );
    expect(css).not.toContain(".composer-goal__action:nth-of-type(2)");
    expect(css).not.toContain(".composer-goal__action:nth-of-type(3)");
  });
});

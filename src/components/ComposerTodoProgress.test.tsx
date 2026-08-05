import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ComposerTodoProgress,
  composerTodoStep,
} from "./ComposerTodoProgress";

describe("ComposerTodoProgress", () => {
  it("在输入框进度控件中展示结构化计划和当前步骤", () => {
    const items = [
      { content: "启动长时间测试任务", status: "completed" },
      { content: "保持任务运行并观察 Todo 界面", status: "in_progress" },
      { content: "结束测试并确认显示结果", status: "pending" },
    ];
    const html = renderToString(
      <ComposerTodoProgress
        locale="zh"
        todos={{ revision: 2, items }}
      />,
    );

    expect(composerTodoStep(items)).toBe(2);
    expect(html).toContain("启动长时间测试任务");
    expect(html).toContain("保持任务运行并观察 Todo 界面");
    expect(html).toContain("第 2 / 3 步");
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("TodoWrite");
  });

  it("没有计划时不占用输入框上方空间", () => {
    expect(
      renderToString(
        <ComposerTodoProgress
          locale="zh"
          todos={{ revision: 0, items: [] }}
        />,
      ),
    ).toBe("");
  });
});

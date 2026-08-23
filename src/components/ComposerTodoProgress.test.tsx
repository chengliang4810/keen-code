import { readFileSync } from "node:fs";
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
    expect(html).not.toContain("composer-todo__step-icon");
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

  it("通过鼠标悬浮和键盘聚焦展示任务详情", () => {
    const source = readFileSync(
      new URL("./ComposerTodoProgress.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("onMouseEnter={() => setOpen(true)}");
    expect(source).toContain("onMouseLeave={() => setOpen(false)}");
    expect(source).toContain("onFocus={() => setOpen(true)}");
    expect(source).not.toContain("onClick=");
  });

  it("进行中图标始终保留旋转动画", () => {
    const css = readFileSync(
      new URL("../styles/app.css", import.meta.url),
      "utf8",
    );

    expect(css).toContain("animation: composer-todo-spin 1.1s linear infinite;");
    expect(css).not.toMatch(
      /@media \(prefers-reduced-motion: reduce\) \{\s*\.composer-todo__item--in_progress/,
    );
  });
});

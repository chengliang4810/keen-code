import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ComposerTodoProgress,
  composerTodoStep,
  shouldCloseComposerTodoPanel,
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

  it("只在点击待办事项面板以外时关闭", () => {
    const inside = {} as EventTarget;
    const outside = {} as EventTarget;
    const panel = {
      contains: (target: Node | null) => target === (inside as Node),
    } as Pick<HTMLElement, "contains">;

    expect(shouldCloseComposerTodoPanel(panel, inside)).toBe(false);
    expect(shouldCloseComposerTodoPanel(panel, outside)).toBe(true);
    expect(shouldCloseComposerTodoPanel(null, outside)).toBe(false);
  });

  it("在捕获阶段监听 pointerdown，避免外部控件阻止事件冒泡", () => {
    const source = readFileSync(
      new URL("./ComposerTodoProgress.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toMatch(
      /document\.addEventListener\(\s*"pointerdown",\s*handleDocumentPointerDown,\s*true,?\s*\)/s,
    );
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

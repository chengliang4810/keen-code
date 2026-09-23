import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ComposerTodoCardList,
  ComposerTodoProgress,
  composerTodoDisplayStatus,
  composerTodoStep,
} from "./ComposerTodoProgress";
import { readCssSource } from "../test-utils/readCssSource";

describe("ComposerTodoProgress", () => {
  it("在输入框进度控件中展示当前步骤", () => {
    const items = [
      { content: "启动长时间测试任务", status: "completed" },
      { content: "保持任务运行并观察 Todo 界面", status: "in_progress" },
      { content: "结束测试并确认显示结果", status: "pending" },
    ];
    const html = renderToString(
      <ComposerTodoProgress
        locale="zh"
        todos={{ revision: 2, items }}
        running
      />,
    );

    expect(composerTodoStep(items)).toBe(2);
    expect(html).toContain("第 2 / 3 步");
    expect(html).toContain('class="composer-todo__progress"');
    expect(html).toContain('stroke-dasharray="33.33333333333333 100"');
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("TodoWrite");
  });

  it("悬浮卡片列出全部计划项", () => {
    const items = [
      { content: "启动长时间测试任务", status: "completed" },
      { content: "保持任务运行并观察 Todo 界面", status: "in_progress" },
      { content: "结束测试并确认显示结果", status: "pending" },
    ];
    const html = renderToString(
      <ComposerTodoCardList items={items} running revision={2} />,
    );

    expect(html).toContain("启动长时间测试任务");
    expect(html).toContain("保持任务运行并观察 Todo 界面");
    expect(html).toContain("composer-todo__item--in_progress");
  });

  it("Turn 结束后不再把遗留的 in_progress Todo 显示为运行中", () => {
    expect(composerTodoDisplayStatus("in_progress", false)).toBe("pending");
    expect(composerTodoDisplayStatus("in_progress", true)).toBe("in_progress");
    expect(composerTodoDisplayStatus("completed", false)).toBe("completed");

    const html = renderToString(
      <ComposerTodoCardList
        items={[{ content: "等待下一轮继续", status: "in_progress" }]}
        running={false}
        revision={2}
      />,
    );

    expect(html).toContain("composer-todo__item--pending");
    expect(html).not.toContain("composer-todo__item--in_progress");
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

  it("详情展示交给 Appica PreviewCard 托管，不再手写悬浮状态机", () => {
    // 悬浮展开/收起语义（hover、focus、Escape、离焦关闭）由框架组件托管，
    // 组件内不得重新出现手写的 open state 事件处理。
    const source = readFileSync(
      new URL("./ComposerTodoProgress.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("PreviewCardTrigger");
    expect(source).toContain("PreviewCardContent");
    expect(source).not.toContain("onMouseEnter");
    expect(source).not.toContain("onMouseLeave");
    expect(source).not.toContain("onFocus={");
    expect(source).not.toContain("onBlur=");
    expect(source).not.toContain("onClick=");
  });

  it("进行中图标始终保留旋转动画", () => {
    const css = readCssSource(new URL("../styles/app.css", import.meta.url));

    expect(css).toContain("animation: composer-todo-spin 1.1s linear infinite;");
    expect(css).not.toMatch(
      /@media \(prefers-reduced-motion: reduce\) \{\s*\.composer-todo__item--in_progress/,
    );
  });
});

import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { GoalTodoControls, isGoalStatus } from "./GoalTodoControls";

describe("GoalTodoControls status Select", () => {
  it("使用分组 Select 取代原生下拉，并保留状态可访问名称", () => {
    const source = readFileSync(
      new URL("./GoalTodoControls.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain('from "@/components/ui/select"');
    expect(source).not.toMatch(/<select(?:\s|>)/);
    expect(source).toContain("<SelectGroup>");
    expect(source).toContain("<SelectLabel>{labels.status}</SelectLabel>");
    expect(source).toContain("aria-label={labels.status}");
  });

  it("只接受 Goal 合法状态，拒绝未知值", () => {
    expect(isGoalStatus("active")).toBe(true);
    expect(isGoalStatus("blocked")).toBe(true);
    expect(isGoalStatus("completed")).toBe(true);
    expect(isGoalStatus("")).toBe(false);
    expect(isGoalStatus("pending")).toBe(false);
  });

  it("渲染状态 combobox 并在忙碌时保持控件语义", () => {
    const html = renderToStaticMarkup(
      <GoalTodoControls
        locale="zh"
        sessionId="session-1"
        goal={{
          revision: 2,
          goal: {
            id: "goal-1",
            title: "完成设置",
            scope: "project",
            status: "active",
            description: "",
            progress_percent: 25,
            created_at: "2026-08-20T00:00:00Z",
            updated_at: "2026-08-20T00:00:00Z",
            objective: "完成设置",
            token_budget: null,
            tokens_used: 10,
            time_used_seconds: 1,
            blocked_reason: null,
          },
        }}
        showTodos={false}
        onError={vi.fn()}
      />,
    );

    expect(html).toContain('role="combobox"');
    expect(html).toContain('aria-label="状态"');
  });
});

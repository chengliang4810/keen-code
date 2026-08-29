import { describe, expect, it } from "vitest";
import {
  classifyToolKind,
  isContextToolKind,
  isGoalToolName,
  isPlanToolName,
  summarizeToolDisplay,
  summarizeCompletedTools,
  summarizeRunningTool,
  toolDetailTail,
} from "./toolDisplay";

describe("toolDisplay", () => {
  it("classifies bash / read / edit / search", () => {
    expect(classifyToolKind("Bash")).toBe("bash");
    expect(classifyToolKind("Read")).toBe("read");
    expect(classifyToolKind("Edit")).toBe("edit");
    expect(classifyToolKind("grep")).toBe("search");
    expect(isContextToolKind("Read")).toBe(true);
    expect(isContextToolKind("Edit")).toBe(false);
  });

  it("将网页、元工具、Skill 和提问与代码工具分开", () => {
    expect(classifyToolKind("WebSearch")).toBe("web");
    expect(classifyToolKind("WebFetch")).toBe("web");
    expect(classifyToolKind("SearchExtraTools")).toBe("meta");
    expect(classifyToolKind("ExecuteExtraTool")).toBe("meta");
    expect(classifyToolKind("SkillTool")).toBe("skill");
    expect(classifyToolKind("DiscoverSkillsTool")).toBe("skill");
    expect(classifyToolKind("AskUserQuestion")).toBe("ask");
  });

  it.each(["FollowupAgent", "InterruptAgent", "AgentResult"])(
    "%s 识别为子 Agent 生命周期工具",
    (name) => expect(classifyToolKind(name)).toBe("subagent"),
  );

  it("WaitAgent 使用独立等待分类和完成摘要", () => {
    expect(classifyToolKind("WaitAgent")).toBe("wait");
    expect(summarizeCompletedTools([{ kind: "WaitAgent" }], "zh")).toBe(
      "等待了子 Agent",
    );
    expect(summarizeCompletedTools([
      { kind: "WaitAgent", waitOutcome: "timeout", waitTaskTitles: ["核对项目结构"] },
    ], "zh")).toBe("等待超时，「核对项目结构」仍在运行");
    expect(summarizeCompletedTools([
      { kind: "WaitAgent", waitOutcome: "agent_state_changed" },
    ], "zh")).toBe("子 Agent 状态已变化");
    expect(summarizeCompletedTools([
      { kind: "WaitAgent", waitOutcome: "user_input" },
    ], "zh")).toBe("等待因用户输入而结束");
    expect(summarizeCompletedTools([
      { kind: "WaitAgent", waitOutcome: "turn_cancelled" },
    ], "zh")).toBe("等待已取消");
    expect(summarizeCompletedTools([
      { kind: "WaitAgent", waitOutcome: "no_running_agents" },
    ], "zh")).toBe("没有正在运行的子 Agent");
  });

  it("summarizes path basename", () => {
    const d = summarizeToolDisplay({
      kind: "Read",
      path: "/Users/me/proj/src/lib/session.ts",
    });
    expect(d.summary).toBe("session.ts");
    expect(d.isContext).toBe(true);
  });

  it("按最后一个运行工具生成包含目标的中文描述", () => {
    expect(
      summarizeRunningTool(
        { kind: "Read", input: '{"file_path":"src/App.tsx"}' },
        "zh",
      ),
    ).toBe("正在读取 App.tsx");
    expect(
      summarizeRunningTool(
        {
          kind: "Bash",
          input:
            '{"command":"rg -n \\\"terminal_create|terminalCreate|terminals\\\" src"}',
        },
        "zh",
      ),
    ).toBe('正在运行 rg -n "terminal_create|terminalCreate|terminals" src');
    expect(
      summarizeRunningTool(
        { kind: "WebSearch", input: '{"query":"Tauri 内存占用"}' },
        "zh",
      ),
    ).toBe("正在使用网页 Tauri 内存占用");
    expect(
      summarizeRunningTool(
        {
          kind: "ExecuteExtraTool",
          input: '{"tool_name":"CronCreate","params":{}}',
        },
        "zh",
      ),
    ).toBe("正在调用工具 CronCreate");
  });

  it("WaitAgent 显示正在等待的子任务标题", () => {
    expect(
      summarizeRunningTool(
        { kind: "WaitAgent", waitTaskTitles: ["代码审查"] },
        "zh",
      ),
    ).toBe("正在等待「代码审查」完成…");
    expect(
      summarizeRunningTool(
        {
          kind: "WaitAgent",
          waitTaskTitles: ["代码审查", "测试验证", "文档检查"],
        },
        "zh",
      ),
    ).toBe("正在等待「代码审查」等 3 个子任务完成…");
    expect(summarizeRunningTool({ kind: "WaitAgent" }, "zh")).toBe(
      "正在等待子任务完成…",
    );
  });

  it("按首次出现顺序汇总历史工具类型", () => {
    expect(
      summarizeCompletedTools(
        [
          { kind: "Edit" },
          { kind: "Write" },
          { kind: "Bash" },
        ],
        "zh",
      ),
    ).toBe("编辑了文件、运行了命令");
  });

  it("识别由输入框专用状态界面承载的 Plan 与 Goal 工具", () => {
    expect(isPlanToolName("TodoWrite")).toBe(true);
    expect(isPlanToolName("tool", "Update Plan")).toBe(true);
    expect(isGoalToolName("goal")).toBe(true);
    expect(isGoalToolName("tool", "create_goal")).toBe(true);
    expect(isGoalToolName("Bash", "检查 goal 状态")).toBe(false);
  });

  it("toolDetailTail keeps last N lines", () => {
    const detail = Array.from({ length: 12 }, (_, i) => `line${i}`).join("\n");
    const tail = toolDetailTail(detail, 3);
    expect(tail).toBe("line9\nline10\nline11");
  });
});

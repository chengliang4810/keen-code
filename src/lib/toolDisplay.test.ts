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
    expect(classifyToolKind("ToolSearch")).toBe("meta");
    expect(classifyToolKind("ExecuteExtraTool")).toBe("meta");
    expect(classifyToolKind("Skill")).toBe("skill");
    expect(classifyToolKind("PluginCommand")).toBe("plugin-command");
    expect(classifyToolKind("AskUser")).toBe("ask");
  });

  it("正式命令名和 ACP other 标题归入 bash，插件仍保持独立分类", () => {
    for (const [kind, title] of [
      ["Git", undefined],
      ["PowerShell", undefined],
      ["Bash", undefined],
      ["other", "Git"],
      ["other", "PowerShell"],
    ] as const) {
      expect(classifyToolKind(kind, title)).toBe("bash");
    }
    expect(classifyToolKind("other", "PluginCommand")).toBe("plugin-command");
    expect(classifyToolKind("other", "ExternalTool")).toBe("fallback");
    expect(
      summarizeRunningTool(
        {
          kind: "other",
          title: "PluginCommand",
          input: JSON.stringify({
            name: "plugin:fixture:native-ext:review",
            args: ["status", "--short"],
          }),
        },
        "zh",
      ),
    ).toContain("正在加载插件命令 plugin:fixture:native-ext:review");
  });

  it("从 Git args 和 PowerShell command 生成安全命令摘要", () => {
    const git = summarizeRunningTool(
      {
        kind: "other",
        title: "Git",
        input: JSON.stringify({ args: ["status", "--short"] }),
      },
      "zh",
    );
    expect(git).toContain("git status --short");
    expect(git).not.toContain('{"args"');

    const powershell = summarizeRunningTool(
      {
        kind: "other",
        title: "PowerShell",
        input: JSON.stringify({ command: 'Write-Output "ok"' }),
      },
      "zh",
    );
    expect(powershell).toContain('Write-Output "ok"');
    expect(powershell).not.toContain('{"command"');
  });

  it("未知工具的 args 不伪装成 Git 命令", () => {
    const summary = summarizeRunningTool(
      {
        kind: "other",
        title: "ExternalTool",
        input: JSON.stringify({ args: ["status", "--short"] }),
      },
      "zh",
    );
    expect(classifyToolKind("other", "ExternalTool")).toBe("fallback");
    expect(summary).not.toContain("git status --short");
  });

  it.each([
    ["Skill", { name: "native-project" }, "正在使用 Skill native-project"],
    ["PluginCommand", { name: "plugin:fixture:native-ext:review", arguments: "private-input" }, "正在加载插件命令 plugin:fixture:native-ext:review"],
    ["ToolSearch", { query: "mcp_resource" }, "正在调用工具 mcp_resource"],
    ["AskUser", { questions: [{ id: "choice", prompt: "选择实现范围", options: [] }] }, "正在询问用户 选择实现范围"],
  ])("%s 按当前输入契约生成活动摘要", (title, input, expected) => {
    expect(summarizeRunningTool({ kind: "other", title: String(title), input: JSON.stringify(input), detail: "raw-result-secret" }, "zh")).toBe(expected);
  });

  it("旧扩展别名不再拥有内置语义，标准 ACP 分类仍有效", () => {
    for (const name of ["SkillTool", "DiscoverSkillsTool", "SearchExtraTools", "AskUserQuestion"]) {
      expect(classifyToolKind(name)).toBe("fallback");
    }
    expect(classifyToolKind("read", "external-file-viewer")).toBe("read");
    expect(summarizeCompletedTools([{ title: "Skill" }, { title: "PluginCommand" }], "zh")).toBe("使用了 Skill、加载了插件命令");
  });

  it.each(["spawn_agent", "send_message", "followup_task", "interrupt_agent"])(
    "%s 识别为子 Agent 生命周期工具",
    (name) => expect(classifyToolKind(name)).toBe("subagent"),
  );

  it("wait_agent 使用独立等待分类和完成摘要", () => {
    expect(classifyToolKind("wait_agent")).toBe("wait");
    expect(summarizeCompletedTools([{ kind: "wait_agent" }], "zh")).toBe(
      "等待了子 Agent",
    );
    expect(summarizeCompletedTools([
      { kind: "wait_agent", waitOutcome: "timed_out", waitTaskTitles: ["核对项目结构"] },
    ], "zh")).toBe("等待 「核对项目结构」 超时");
    expect(summarizeCompletedTools([
      { kind: "wait_agent", waitOutcome: "mailbox_activity" },
    ], "zh")).toBe("Agent 邮箱已有新消息");
    expect(summarizeCompletedTools([
      { kind: "wait_agent", waitOutcome: "user_steer_activity" },
    ], "zh")).toBe("收到用户追加消息");
    expect(summarizeCompletedTools([
      { kind: "wait_agent", waitOutcome: "turn_ended" },
    ], "zh")).toBe("等待期间 Turn 已结束");
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

  it("wait_agent 显示正在等待的子任务标题", () => {
    expect(
      summarizeRunningTool(
        { kind: "wait_agent", waitTaskTitles: ["代码审查"] },
        "zh",
      ),
    ).toBe("正在等待「代码审查」完成…");
    expect(
      summarizeRunningTool(
        {
          kind: "wait_agent",
          waitTaskTitles: ["代码审查", "测试验证", "文档检查"],
        },
        "zh",
      ),
    ).toBe("正在等待「代码审查」等 3 个子任务完成…");
    expect(summarizeRunningTool({ kind: "wait_agent" }, "zh")).toBe(
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

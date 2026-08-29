import React from "react";
import { readFileSync } from "node:fs";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  latestSubagentToolCallIds,
  subagentForTool,
  TimelineToolRow,
} from "./TimelineToolRow";
import type { AcpSubagentInfo } from "@/lib/acp/store";

describe("TimelineToolRow", () => {
  const planAgent: AcpSubagentInfo = {
    agent_id: "child-thread-1",
    agent_name: "plan",
    nickname: null,
    status: "running",
    is_background: false,
    started_at: Date.now() - 2_000,
    stopped_at: null,
    result: null,
    segments: [{ kind: "content", text: "正在核对项目结构" }],
  };

  it("Agent 工具按 child_thread_id 渲染为可点击子智能体卡片", () => {
    const tool = {
      kind: "tool" as const,
      toolCallId: "agent-tool-1",
      title: "Agent",
      toolKind: "Agent",
      status: "in_progress",
      input:
        '{"subagent_type":"plan","description":"核对项目结构","name":"unused-alias"}',
      output: "child_thread_id: child-thread-1",
    };
    expect(subagentForTool(tool, [planAgent])).toBe(planAgent);

    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool,
        subagents: [planAgent],
        onOpenResource: () => {},
      }),
    );
    expect(html).toContain("lobe-subagent-card");
    expect(html).toContain(">plan<");
    expect(html).toContain("核对项目结构");
    expect(html).not.toContain("unused-alias");
    expect(html).not.toContain("child_thread_id");
  });

  it("Agent 尚未关联运行时状态时仍显示专用卡片", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "agent-pending-projection",
          title: "Agent",
          toolKind: "Agent",
          status: "in_progress",
          input: JSON.stringify({
            subagent_type: "plan",
            description: "检查登录流程",
          }),
        },
      }),
    );

    expect(html).toContain("lobe-subagent-card");
    expect(html).toContain(">plan<");
    expect(html).toContain("检查登录流程");
    expect(html).toContain('data-agent-status="running"');
    expect(html).not.toContain('data-testid="timeline-tool"');
  });

  it("WaitAgent 显示独立的等待动作而不是通用工具名", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "wait-agent-1",
          title: "WaitAgent",
          toolKind: "WaitAgent",
          status: "completed",
          input: '{"timeout_ms":120000}',
          output:
            '{"outcome":"timeout","running_agents":[{"child_thread_id":"child-thread-1"}]}',
        },
        subagents: [{ ...planAgent, task_title: "核对项目结构" }],
      }),
    );

    expect(html).toContain("等待超时");
    expect(html).toContain("仍在运行");
    expect(html).toContain("核对项目结构");
    expect(html).not.toContain(">WaitAgent<");
  });

  it("运行中的 WaitAgent 显示当前等待的子任务名称", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "wait-agent-running",
          title: "WaitAgent",
          toolKind: "WaitAgent",
          status: "in_progress",
          input: '{"timeout_ms":120000}',
        },
        subagents: [{ ...planAgent, task_title: "核对项目结构" }],
      }),
    );

    expect(html).toContain("等待");
    expect(html).toContain("核对项目结构");
    expect(html).not.toContain("等待</span><span>子 Agent");
  });

  it("FollowupAgent 按 target 关联同一子 Agent 卡片", () => {
    const tool = {
      kind: "tool" as const,
      toolCallId: "followup-agent",
      title: "FollowupAgent",
      toolKind: "FollowupAgent",
      status: "completed",
      input: JSON.stringify({
        target: "child-thread-1",
        message: "补充核验运行证据",
      }),
      output: "",
    };

    expect(subagentForTool(tool, [planAgent])).toBe(planAgent);
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool,
        subagents: [planAgent],
      }),
    );
    expect(html).toContain("lobe-subagent-card");
    expect(html).toContain("补充核验运行证据");
    expect(html).not.toContain("工具 FollowupAgent");
  });

  it("Agent 创建前失败时不被历史状态覆盖", () => {
    const tool = {
      kind: "tool" as const,
      toolCallId: "agent-rejected",
      title: "Agent",
      toolKind: "Agent",
      status: "failed",
      isError: true,
      input: JSON.stringify({
        subagent_type: "vision",
        description: "视觉能力实测",
      }),
      output: "analyze attached images directly instead of calling the vision Agent",
    };
    const latest = latestSubagentToolCallIds([tool], []);
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool,
        isLatestSubagentEvent: latest.has(tool.toolCallId),
      }),
    );

    expect([...latest]).toEqual([]);
    expect(html).toContain('data-agent-status="failed"');
    expect(html).toContain(">失败<");
    expect(html).not.toContain('data-agent-status="history"');
  });

  it("完成卡片仅显示完成标记，不显示耗时和外置状态文案", () => {
    const completedAgent = {
      ...planAgent,
      status: "done" as const,
      stopped_at: Date.now(),
    };
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "agent-done",
          title: "Agent",
          toolKind: "Agent",
          status: "completed",
          input:
            '{"subagent_type":"plan","description":"完成实施计划"}',
          output: "child_thread_id: child-thread-1",
          durationMs: 41_000,
        },
        subagents: [completedAgent],
      }),
    );

    expect(html).toContain('data-agent-status="done"');
    expect(html).toContain("tabler-icon-check");
    expect(html).not.toContain("41秒");
    expect(html).not.toContain("41s");
    expect(html).not.toContain(">已完成<");
  });

  it("Agent 卡片固定宽度并按对话可用宽度自然换行", () => {
    const css = readFileSync(
      new URL("./lobe-chat.css", import.meta.url),
      "utf8",
    );

    expect(css).toMatch(
      /\.lobe-chat-assistant-timeline\s*\{[^}]*display:\s*flex;[^}]*flex-wrap:\s*wrap;/s,
    );
    expect(css).toMatch(
      /\.lobe-timeline-rail--subagent\s*\{[^}]*flex:\s*0 0 min\(100%, 208px\);[^}]*width:\s*min\(100%, 208px\);/s,
    );
  });
  it("计划工具不进入对话工具时间线", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "todo-1",
          title: "TodoWrite",
          toolKind: "TodoWrite",
          status: "completed",
          input: JSON.stringify({
            todos: [
              { content: "检查现有官网文件", status: "in_progress" },
              { content: "重写页面视觉", status: "pending" },
            ],
          }),
        },
      }),
    );

    expect(html).toBe("");
  });

  it("Goal 工具由输入框目标栏承载，不进入对话工具时间线", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "goal-1",
          title: "goal",
          toolKind: "goal",
          status: "completed",
          input: '{"action":"create","objective":"完成测试"}',
        },
      }),
    );

    expect(html).toBe("");
  });

  it("读取工具从 file_path 提取文件名和行号范围且不展示内容", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "read-json",
          title: "Read",
          toolKind: "Read",
          status: "completed",
          input:
            '{"file_path":"/Users/chengliang/code-projects/test/index.html","offset":21,"limit":40}',
          output: "1 <!doctype html>",
        },
      }),
    );

    expect(html).toContain("已读取");
    expect(html).toContain("index.html:21\u201360");
    expect(html).not.toContain("doctype");
    expect(html).not.toContain("file_path");
  });

  it("写入工具从 file_path 提取文件名并隐藏原始参数", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        onOpenResource: () => {},
        tool: {
          kind: "tool",
          toolCallId: "write-json",
          title: "Write",
          toolKind: "Write",
          status: "completed",
          input: '{"file_path":"/tmp/styles.css","content":"body{}"}',
        },
      }),
    );

    expect(html).toContain("已编辑");
    expect(html).toContain("styles.css");
    expect(html).not.toContain("file_path");
    expect(html).not.toContain("body{}");
  });

  it("Bash 工具从 command 提取命令并隐藏 timeout JSON", () => {
    const command = "python3 - <<'PY'\nprint('ok')\nPY";
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "bash-json",
          title: "Bash",
          toolKind: "Bash",
          status: "completed",
          input: JSON.stringify({ command, timeout: 15000 }),
        },
      }),
    );

    expect(html).toContain("已执行");
    expect(html).toContain("python3 - &lt;&lt;&#x27;PY&#x27;");
    expect(html).not.toContain("timeout");
    expect(html).not.toContain("15000");
  });

  it("真实 ACP wire 的 execute kind 将 Bash 识别为命令", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "acp-execute",
          title: "Bash",
          toolKind: "execute",
          status: "completed",
          input: '{"command":"pnpm test"}',
        },
      }),
    );

    expect(html).toContain("已执行");
    expect(html).toContain("pnpm test");
    expect(html).toContain("tabler-icon-code");
  });

  it("真实 ACP wire 的 search kind 从 Grep 和 Glob 标题提取 pattern", () => {
    for (const [title, pattern] of [
      ["Grep", "missing_symbol"],
      ["Glob", "**/*.tsx"],
    ] as const) {
      const html = renderToString(
        React.createElement(TimelineToolRow, {
          locale: "zh",
          tool: {
            kind: "tool",
            toolCallId: `acp-${title.toLowerCase()}`,
            title,
            toolKind: "search",
            status: "completed",
            input: JSON.stringify({ pattern }),
            output: "No matches found.",
          },
        }),
      );

      expect(html).toContain("已搜索");
      expect(html).toContain(pattern);
      expect(html).toContain("tabler-icon-search");
      expect(html).not.toContain("No matches found.");
    }
  });

  it("真实 ACP wire 的 edit kind 从 folder_operations 标题识别目录浏览", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "acp-folder",
          title: "folder_operations",
          toolKind: "edit",
          status: "completed",
          input: '{"folder_path":"/tmp/project"}',
        },
      }),
    );

    expect(html).toContain("已浏览");
    expect(html).toContain(">project<");
    expect(html).toContain("tabler-icon-folder");
    expect(html).not.toContain("已编辑");
  });

  it("目录工具从 folder_path 提取目录名并隐藏目录树", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "folder-json",
          title: "folder_operations",
          toolKind: "folder_operations",
          status: "completed",
          input: JSON.stringify({
            folder_path: "/Users/chengliang/code-projects/test",
            max_depth: 3,
            operation: "deep_scan",
          }),
          output: "📁 test\n├── index.html\n└── styles.css\n\nTotal: 2 entries",
        },
      }),
    );

    expect(html).toContain("已浏览");
    expect(html).toContain(">test<");
    expect(html).toContain("tabler-icon-folder");
    expect(html).not.toContain("index.html");
    expect(html).not.toContain("Total");
    expect(html).not.toContain("folder_path");
  });

  it("Glob 工具从 pattern 提取搜索目标并隐藏原始响应", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "glob-json",
          title: "Glob",
          toolKind: "Glob",
          status: "completed",
          input: JSON.stringify({
            path: "/Users/chengliang/code-projects/test",
            pattern: "**/*.{html,css,js}",
          }),
          output: "No files found.",
        },
      }),
    );

    expect(html).toContain("已搜索");
    expect(html).toContain("**/*.{html,css,js}");
    expect(html).not.toContain("No files found.");
  });

  it("Grep 零匹配时只显示搜索目标且不提供详情面板", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "grep-empty",
          title: "Grep",
          toolKind: "Grep",
          status: "completed",
          input: JSON.stringify({
            path: "/Users/chengliang/code-projects/test",
            pattern: "missing_symbol",
          }),
          output: "No matches found.",
        },
      }),
    );

    expect(html).toContain("已搜索");
    expect(html).toContain("missing_symbol");
    expect(html).not.toContain("No matches found.");
    expect(html).toContain('disabled=""');
    expect(html).not.toContain("aria-expanded");
  });

  it("成功工具默认只显示紧凑证据摘要", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "read-1",
          title: "读取项目文件",
          toolKind: "read",
          status: "completed",
          path: "README.md",
          durationMs: 118,
          structuredResult: {
            output: "读取完成",
            items: [
              {
                type: "file",
                path: "README.md",
                operation: "read",
              },
            ],
          },
        },
      }),
    );

    expect(html).toContain("读取");
    expect(html).toContain("README.md");
    expect(html).not.toContain(">完成<");
    expect(html).toContain("118ms");
    expect(html).toContain('disabled=""');
    expect(html).not.toContain("结构化结果");
  });

  it("失败工具默认折叠并保留可展开的错误详情", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "bash-1",
          title: "运行测试",
          toolKind: "bash",
          status: "failed",
          isError: true,
          durationMs: 1437,
          structuredResult: {
            output: "AssertionError",
            is_error: true,
            items: [
              {
                type: "command",
                command: "pnpm test",
                exit_code: 1,
                stderr: "AssertionError",
              },
            ],
          },
        },
      }),
    );

    expect(html).toContain("已执行");
    expect(html).not.toContain(">失败<");
    expect(html).toContain("1.4s");
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("AssertionError");
  });

  it("运行中的工具仍展示实时状态", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "bash-running",
          title: "Bash",
          toolKind: "Bash",
          status: "in_progress",
          input: '{"command":"pnpm test"}',
        },
      }),
    );

    expect(html).toContain(">运行中<");
  });

  it.each([
    [
      "AskUserQuestion",
      {
        questions: [
          { question: "是否继续提交？", header: "提交", options: [] },
        ],
      },
      "询问用户",
      "是否继续提交？",
    ],
    ["SearchExtraTools", { query: "calendar" }, "查找工具", "calendar"],
    ["SkillTool", { skill_name: "pdf" }, "加载 Skill", "pdf"],
    ["DiscoverSkillsTool", { query: "文档" }, "查找 Skill", "文档"],
  ])("%s 使用针对性的文字摘要", (name, input, action, summary) => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: `semantic-${name}`,
          title: name,
          toolKind: name,
          status: "completed",
          input: JSON.stringify(input),
          output: "不应展开显示的原始工具结果",
        },
      }),
    );

    expect(html).toContain(action);
    expect(html).toContain(summary);
    expect(html).not.toContain("不应展开显示的原始工具结果");
    expect(html).toContain('disabled=""');
  });

  it.each([
    ["WebSearch", { query: "Tauri 内存占用" }, "搜索网页", "Tauri 内存占用"],
    [
      "WebFetch",
      { url: "https://docs.rs/tauri/latest/tauri/" },
      "访问网页",
      "https://docs.rs/tauri/latest/tauri/",
    ],
    [
      "ExecuteExtraTool",
      { tool_name: "CronCreate", params: { schedule: "0 9 * * *" } },
      "调用工具",
      "CronCreate",
    ],
  ])("%s 不再落入错误的通用分类", (name, input, action, summary) => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: `mapped-${name}`,
          title: name,
          toolKind: name,
          status: "completed",
          input: JSON.stringify(input),
          output: "原始结果不直接展示",
        },
      }),
    );

    expect(html).toContain(action);
    expect(html).toContain(summary);
    expect(html).not.toContain("原始结果不直接展示");
    expect(html).not.toContain("已执行");
  });
});

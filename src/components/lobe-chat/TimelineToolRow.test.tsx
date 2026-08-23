import React from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { subagentForTool, TimelineToolRow } from "./TimelineToolRow";
import type { AcpSubagentInfo } from "@/lib/acp/store";

describe("TimelineToolRow", () => {
  const planAgent: AcpSubagentInfo = {
    agent_id: "child-thread-1",
    agent_name: "plan",
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
      input: '{"subagent_type":"plan"}',
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
    expect(html).toContain("lobe-subagent-row");
    expect(html).toContain(">plan<");
    expect(html).toContain("正在核对项目结构");
    expect(html).not.toContain("child_thread_id");
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

  it("读取工具从 file_path 提取文件名且不展示内容和计数", () => {
    const html = renderToString(
      React.createElement(TimelineToolRow, {
        locale: "zh",
        tool: {
          kind: "tool",
          toolCallId: "read-json",
          title: "Read",
          toolKind: "Read",
          status: "completed",
          input: '{"file_path":"/Users/chengliang/code-projects/test/index.html"}',
          output: "1 <!doctype html>",
        },
      }),
    );

    expect(html).toContain("已读取");
    expect(html).toContain("index.html");
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
});

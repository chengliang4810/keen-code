import { describe, expect, it } from "vitest";
import {
  projectPeriStoredMessages,
  projectPeriStoredSubagents,
  projectPeriStoredSubagentThreads,
  withSubagentPrompts,
} from "./periStoredMessages";

describe("projectPeriStoredMessages", () => {
  it("隐藏后台完成 reminder 但保留消息边界和用户正文", () => {
    const messages = projectPeriStoredMessages([
      { id: "user-1", role: "user", content: "开始任务" },
      { id: "assistant-1", role: "assistant", content: "等待结果" },
      {
        id: "reminder-1",
        role: "user",
        content: "<system-reminder>\nAgent 已完成\n</system-reminder>",
      },
      { id: "assistant-2", role: "assistant", content: "结果已吸收" },
      {
        id: "user-2",
        role: "user",
        content: "请解释 <system-reminder> 这个标签",
      },
    ]);

    expect(messages.map(({ role, content }) => ({ role, content }))).toEqual([
      { role: "user", content: "开始任务" },
      { role: "assistant", content: "等待结果" },
      { role: "assistant", content: "结果已吸收" },
      { role: "user", content: "请解释 <system-reminder> 这个标签" },
    ]);
  });

  it("从持久化子 Thread 恢复完整调用时间线", () => {
    const [agent] = projectPeriStoredSubagentThreads([
      {
        id: "child-1",
        name: "verification",
        nickname: { index: 3, generation: 1 },
        status: "done",
        createdAt: "2026-08-24T03:29:27Z",
        updatedAt: "2026-08-24T03:34:17Z",
        messages: [
          { id: "user-1", role: "user", content: "检查代码" },
          {
            id: "assistant-1",
            role: "assistant",
            content: [{ type: "reasoning", text: "先搜索" }],
            tool_calls: [{ id: "grep-1", name: "Grep", arguments: { pattern: "TODO" } }],
          },
          { id: "tool-1", role: "tool", tool_call_id: "grep-1", content: "无匹配", is_error: false },
          { id: "assistant-2", role: "assistant", content: "检查完成" },
        ],
      },
    ]);

    expect(agent).toEqual(
      expect.objectContaining({
        agent_id: "child-1",
        agent_name: "verification",
        nickname: { index: 3, generation: 1 },
        status: "done",
        result: "检查完成",
      }),
    );
    expect(agent?.segments).toEqual([
      { kind: "thought", text: "先搜索" },
      expect.objectContaining({ kind: "tool", toolCallId: "grep-1", status: "completed" }),
      { kind: "content", text: "检查完成" },
    ]);
  });

  it("从历史 Agent 调用恢复可点击的子 Agent 投影", () => {
    const messages = projectPeriStoredMessages([
      {
        id: "assistant-1",
        role: "assistant",
        content: "",
        tool_calls: [
          {
            id: "agent-1",
            name: "Agent",
            arguments: {
              prompt: "你是一个架构规划（plan）智能体",
              name: "不应成为类型",
            },
          },
        ],
      },
      {
        id: "result-1",
        role: "tool",
        content: "child_thread_id: child-123\n# 实施计划\n完成",
        tool_call_id: "agent-1",
        is_error: false,
      },
    ]);

    expect(projectPeriStoredSubagents(messages)).toEqual([
      expect.objectContaining({
        agent_id: "child-123",
        agent_name: "plan",
        prompt: "你是一个架构规划（plan）智能体",
        status: "done",
        result: "# 实施计划\n完成",
        segments: [{ kind: "content", text: "# 实施计划\n完成" }],
      }),
    ]);
  });

  it("用实时 Agent 工具输入补齐委派任务", () => {
    const agents = withSubagentPrompts(
      [
        {
          id: "assistant-1",
          role: "assistant",
          content: "",
          segments: [
            {
              kind: "tool",
              toolCallId: "agent-1",
              title: "Agent",
              toolKind: "Agent",
              status: "pending",
              streaming: false,
              input:
                '{"subagent_type":"plan","description":"检查权限","prompt":"只读检查权限实现"}',
            },
          ],
        },
      ],
      [
        {
          agent_id: "child-1",
          agent_name: "plan",
          nickname: null,
          status: "running",
          is_background: false,
          started_at: 1,
          stopped_at: null,
          result: null,
          segments: [],
        },
      ],
    );

    expect(agents[0]?.prompt).toBe("只读检查权限实现");
    expect(agents[0]?.task_title).toBe("检查权限");
  });

  it("按启动顺序匹配同类型并行 Agent 的委派任务", () => {
    const messages = ["检查界面", "检查数据链"].map((prompt, index) => ({
      id: `assistant-${index}`,
      role: "assistant" as const,
      content: "",
      segments: [{
        kind: "tool" as const,
        toolCallId: `agent-${index}`,
        title: "Agent",
        toolKind: "Agent",
        status: "pending" as const,
        streaming: false,
        input: JSON.stringify({ subagent_type: "explorer", prompt }),
      }],
    }));
    const agents = ["child-1", "child-2"].map((agent_id) => ({
      agent_id,
      agent_name: "explorer",
      nickname: null,
      status: "running" as const,
      is_background: true,
      started_at: 1,
      stopped_at: null,
      result: null,
      segments: [],
    }));

    expect(withSubagentPrompts(messages, agents).map((agent) => agent.prompt)).toEqual([
      "检查界面",
      "检查数据链",
    ]);
  });

  it("保留思考、工具和正文的真实顺序并回填工具结果", () => {
    const messages = projectPeriStoredMessages([
      {
        id: "user-1",
        role: "user",
        content: "检查项目",
      },
      {
        id: "assistant-1",
        role: "assistant",
        content: [
          { type: "reasoning", text: "先读取" },
          {
            type: "tool_use",
            id: "tool-1",
            name: "Read",
            input: { file_path: "README.md" },
          },
          { type: "text", text: "已完成" },
        ],
      },
      {
        id: "result-1",
        role: "tool",
        content: "README 内容",
        tool_call_id: "tool-1",
        is_error: false,
      },
    ]);

    expect(messages).toHaveLength(2);
    expect(messages[1]?.segments).toEqual([
      { kind: "thought", text: "先读取" },
      {
        kind: "tool",
        toolCallId: "tool-1",
        title: "Read",
        toolKind: "Read",
        status: "completed",
        streaming: false,
        input: '{"file_path":"README.md"}',
        output: "README 内容",
        detail: "README 内容",
      },
      { kind: "content", text: "已完成" },
    ]);
  });

  it("保留没有匹配调用的失败工具结果", () => {
    const messages = projectPeriStoredMessages([
      {
        id: "result-1",
        role: "tool",
        content: "permission denied",
        tool_call_id: "tool-1",
        is_error: true,
      },
    ]);

    expect(messages[0]).toMatchObject({
      role: "tool",
      marker: "tool_step",
      toolCallId: "tool-1",
      toolStatus: "failed",
      isError: true,
    });
  });

  it("把外层 tool_calls（chatcmpl 存储）投影为工具段并回填结果", () => {
    const messages = projectPeriStoredMessages([
      {
        id: "user-1",
        role: "user",
        content: "生成过山车页面",
      },
      {
        id: "assistant-1",
        role: "assistant",
        content: [
          { type: "reasoning", text: "先看现有文件" },
          { type: "text", text: "开始读取" },
        ],
        tool_calls: [
          {
            id: "chatcmpl-tool-9b268c64",
            name: "Read",
            arguments: { file_path: "index.html" },
          },
        ],
      },
      {
        id: "result-1",
        role: "tool",
        content: "total 72\ndrwxr-xr-x",
        tool_call_id: "chatcmpl-tool-9b268c64",
        is_error: false,
      },
    ]);

    expect(messages).toHaveLength(2);
    const assistant = messages[1]!;
    expect(assistant.role).toBe("assistant");
    const tool = assistant.segments?.find((segment) => segment.kind === "tool");
    expect(tool).toMatchObject({
      kind: "tool",
      toolCallId: "chatcmpl-tool-9b268c64",
      title: "Read",
      toolKind: "Read",
      status: "completed",
      input: '{"file_path":"index.html"}',
      output: "total 72\ndrwxr-xr-x",
    });
    // 正文与思考不受外层 tool_calls 影响。
    expect(assistant.content).toBe("开始读取");
    expect(assistant.thought).toBe("先看现有文件");
  });

  it("非法 tool_calls 项跳过且与 tool_use 块去重", () => {
    const messages = projectPeriStoredMessages([
      {
        id: "assistant-1",
        role: "assistant",
        content: [
          {
            type: "tool_use",
            id: "tool-1",
            name: "Bash",
            input: { command: "ls" },
          },
        ],
        tool_calls: [
          { id: "tool-1", name: "Bash", arguments: { command: "ls" } },
          { name: "缺 id 的非法项" },
          { id: "", name: "空 id" },
        ],
      },
    ]);

    const tools = messages[0]!.segments?.filter(
      (segment) => segment.kind === "tool",
    );
    expect(tools).toHaveLength(1);
    expect(tools?.[0]).toMatchObject({ toolCallId: "tool-1", title: "Bash" });
  });

  it("同一回合内工具调用之间的多条 assistant 行合并为一条消息", () => {
    const messages = projectPeriStoredMessages([
      { id: "user-1", role: "user", content: "修复页面" },
      {
        id: "assistant-1",
        role: "assistant",
        content: [{ type: "text", text: "先读取文件。" }],
        tool_calls: [{ id: "tool-1", name: "Read", arguments: {} }],
      },
      {
        id: "result-1",
        role: "tool",
        content: "文件内容",
        tool_call_id: "tool-1",
        is_error: false,
      },
      {
        id: "assistant-2",
        role: "assistant",
        content: [{ type: "text", text: "接下来执行脚本。" }],
        tool_calls: [{ id: "tool-2", name: "Bash", arguments: {} }],
      },
      {
        id: "result-2",
        role: "tool",
        content: "脚本输出",
        tool_call_id: "tool-2",
        is_error: false,
      },
      {
        id: "assistant-3",
        role: "assistant",
        content: [{ type: "text", text: "修复完成。" }],
      },
      { id: "user-2", role: "user", content: "谢谢" },
      {
        id: "assistant-4",
        role: "assistant",
        content: [{ type: "text", text: "不客气。" }],
      },
    ]);

    // 两个用户回合各只有一条 assistant 消息——操作按钮行每回合只出现一次。
    expect(messages.map((m) => m.role)).toEqual([
      "user",
      "assistant",
      "user",
      "assistant",
    ]);
    const firstTurn = messages[1]!;
    expect(firstTurn.id).toBe("assistant-1");
    expect(firstTurn.segments?.map((s) => s.kind)).toEqual([
      "content",
      "tool",
      "content",
      "tool",
      "content",
    ]);
    expect(firstTurn.content).toBe("先读取文件。接下来执行脚本。修复完成。");
    const tools = firstTurn.segments!.filter((s) => s.kind === "tool");
    expect(tools).toHaveLength(2);
    expect(tools[0]).toMatchObject({
      toolCallId: "tool-1",
      status: "completed",
      output: "文件内容",
    });
    expect(tools[1]).toMatchObject({
      toolCallId: "tool-2",
      status: "completed",
      output: "脚本输出",
    });
    expect(messages[3]!.content).toBe("不客气。");
  });
});

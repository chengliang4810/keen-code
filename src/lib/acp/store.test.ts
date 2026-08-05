import { describe, expect, it } from "vitest";
import {
  createAcpWorkspaceState,
  emptySession,
  reduceAgentEvent,
  reduceRecovery,
  reduceSessionUpdate,
  type AcpSessionView,
} from "./store";
import {
  commitLiveTurnToHistory,
  reduceReplayedSessionUpdate,
} from "./projection";

function makeView(): AcpSessionView {
  const state = createAcpWorkspaceState();
  state.sessions["s1"] = emptySession("s1");
  return state.sessions["s1"]!;
}

describe("acp store reducer", () => {
  it("完成时补齐 peri 未回送的用户消息并保持用户在助手之前", () => {
    const view = makeView();
    view.live_segments = [
      { kind: "content", text: "Hello! How can I help you today?" },
    ];

    commitLiveTurnToHistory(view, { userContent: "hello" });

    expect(view.history).toEqual([
      { role: "user", content: "hello" },
      {
        role: "assistant",
        content: "Hello! How can I help you today?",
        segments: [
          { kind: "content", text: "Hello! How can I help you today?" },
        ],
      },
    ]);
    expect(view.live_segments).toEqual([]);
  });

  it("完成时不重复已有的 user_message_chunk", () => {
    const view = makeView();
    view.history.push({ role: "user", content: "hello" });
    view.live_segments = [{ kind: "content", text: "Hi" }];

    commitLiveTurnToHistory(view, { userContent: "hello" });

    expect(view.history).toEqual([
      { role: "user", content: "hello" },
      {
        role: "assistant",
        content: "Hi",
        segments: [{ kind: "content", text: "Hi" }],
      },
    ]);
  });

  it("完成时保留思考正文和处理耗时供折叠查看", () => {
    const view = makeView();
    view.live_segments = [
      { kind: "thought", text: "先检查事件时序，再确认状态投影。" },
      { kind: "content", text: "已完成修复。" },
    ];

    commitLiveTurnToHistory(view, {
      userContent: "修复进行中状态",
      thinkingDurationMs: 122_000,
    });

    expect(view.history[1]).toEqual({
      role: "assistant",
      content: "已完成修复。",
      thought: "先检查事件时序，再确认状态投影。",
      segments: [
        { kind: "thought", text: "先检查事件时序，再确认状态投影。" },
        { kind: "content", text: "已完成修复。" },
      ],
      thinkingDurationMs: 122_000,
    });
  });

  it("完成后把工具固化到本轮历史并清空实时工具", () => {
    const view = makeView();
    view.live_segments = [{
      kind: "tool",
      toolCallId: "tool-1",
      title: "Read",
      toolKind: "Read",
      status: "completed",
      detail: "ok",
      input: '{"file_path":"README.md"}',
      output: "ok",
      streaming: false,
      isError: false,
    }];

    commitLiveTurnToHistory(view);

    expect(view.history[0]?.segments).toEqual([
      {
        kind: "tool",
        toolCallId: "tool-1",
        title: "Read",
        toolKind: "Read",
        status: "completed",
        detail: "ok",
        input: '{"file_path":"README.md"}',
        output: "ok",
        streaming: false,
        isError: false,
      },
    ]);
    expect(view.live_segments).toEqual([]);
  });

  it("重放时把思考正文归入后续 Assistant 消息", () => {
    const view = makeView();
    reduceReplayedSessionUpdate(view, {
      sessionUpdate: "agent_thought_chunk",
      content: { type: "text", text: "检查历史。" },
    });
    reduceReplayedSessionUpdate(view, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "检查完成。" },
    });
    commitLiveTurnToHistory(view);

    expect(view.history).toEqual([
      {
        role: "assistant",
        content: "检查完成。",
        thought: "检查历史。",
        segments: [
          { kind: "thought", text: "检查历史。" },
          { kind: "content", text: "检查完成。" },
        ],
      },
    ]);
    expect(view.live_segments).toEqual([]);
  });

  it("归约 agent_message_chunk 到 text", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "你好" },
    });
    expect(view.live_segments).toEqual([{ kind: "content", text: "你好" }]);
  });

  it("归约 user_message_chunk 到 history", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "帮我重构" },
    });
    expect(view.history).toEqual([{ role: "user", content: "帮我重构" }]);
  });

  it("归约 tool_call 与 tool_call_update", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call",
      toolCallId: "tc-1",
      title: "Bash",
      rawInput: { cmd: "ls" },
    });
    expect(view.live_segments[0]).toMatchObject({
      kind: "tool",
      title: "Bash",
      input: '{"cmd":"ls"}',
    });

    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-1",
      status: "completed",
      rawOutput: "file.txt",
    });
    expect(view.live_segments[0]).toMatchObject({
      kind: "tool",
      status: "completed",
      output: "file.txt",
      streaming: false,
    });
  });

  it("严格保留正文、工具、正文的 ACP 到达顺序", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "先说明。" },
    });
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call",
      toolCallId: "tc-order",
      title: "Read",
      rawInput: { path: "README.md" },
    });
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-order",
      status: "completed",
      rawOutput: "ok",
    });
    reduceSessionUpdate(view, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "再总结。" },
    });

    commitLiveTurnToHistory(view);

    expect(view.history[0]?.segments?.map((segment) => segment.kind)).toEqual([
      "content",
      "tool",
      "content",
    ]);
    expect(view.history[0]?.content).toBe("先说明。再总结。");
  });

  it("归约 plan 到 todos（只读）", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "plan",
      entries: [
        { content: "写测试", priority: "medium", status: "completed" },
        { content: "跑构建", priority: "medium", status: "pending" },
      ],
    });
    expect(view.todos.items).toHaveLength(2);
    expect(view.todos.items[0]).toEqual({ content: "写测试", status: "completed" });
  });

  it("归约 goal_changed 事件", () => {
    const view = makeView();
    reduceAgentEvent(view, {
      type: "goal_changed",
      value: {
        revision: 2,
        change: "updated",
        goal: {
          id: "goal-1",
          title: "Ship v2",
          scope: "project",
          status: "active",
          objective: "Ship v2",
          tokens_used: 0,
          time_used_seconds: 0,
          created_at: "2026-08-01T00:00:00Z",
          updated_at: "2026-08-01T00:00:00Z",
        },
      },
    });
    expect(view.goal.revision).toBe(2);
    expect(view.goal.goal?.title).toBe("Ship v2");
  });

  it("归约 subagent_started / stopped", () => {
    const view = makeView();
    reduceAgentEvent(view, {
      type: "subagent_started",
      value: {
        agent_name: "explorer",
        instance_id: "sa-1",
        is_background: false,
      },
    });
    expect(view.subagents).toHaveLength(1);
    expect(view.subagents[0]!.status).toBe("running");

    reduceAgentEvent(view, {
      type: "subagent_stopped",
      value: {
        agent_name: "explorer",
        result: "done",
        is_error: false,
        instance_id: "sa-1",
      },
    });
    expect(view.subagents[0]!.status).toBe("done");
  });

  it("归约 recovery 通知含 pending_tools", () => {
    const view = makeView();
    reduceRecovery(view, {
      status: "restoring",
      cursor: { epoch: "e-1", sequence: 5 },
      pending_tools: [
        {
          call_id: "tc-crash",
          name: "Bash",
          status: "unknown_outcome",
          started_at_unix_ms: 1000,
        },
      ],
    });
    expect(view.replay.restoring).toBe(true);
    expect(view.replay.pending_tools).toHaveLength(1);
    expect(view.replay.cursor).toEqual({ epoch: "e-1", sequence: 5 });
    expect(view.last_error?.code).toBe("pending_tools");
  });
});

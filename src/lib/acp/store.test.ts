import { describe, expect, it } from "vitest";
import {
  createAcpWorkspaceState,
  emptySession,
  reduceAgentEvent,
  reduceRecovery,
  reduceSessionUpdate,
  resolveSessionUpdateSourceAgentId,
  type AcpSessionView,
} from "./store";
import {
  commitLiveTurnToHistory,
  reduceReplayedSessionUpdate,
  replaceHistoryTurnMetrics,
} from "./projection";
import {
  shouldDriveMainSessionStreaming,
  type SessionUpdateEnvelope,
} from "./events";
import { projectAcpLiveMessage } from "../sessionProjection";

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

  it("完成时把低延迟指标固化到 Assistant 历史", () => {
    const view = makeView();
    view.live_segments = [{ kind: "content", text: "已完成。" }];
    const turnMetrics = {
      turnId: "turn-1",
      sendAcknowledgementMs: 4,
      timeToFirstSseMs: 120,
      timeToFirstVisibleTokenMs: 150,
      totalMs: 900,
      inputTokens: 1_000,
      cacheReadTokens: 250,
      cacheCreationTokens: 0,
      cacheHitRate: 0.25,
    };

    commitLiveTurnToHistory(view, { turnMetrics });

    expect(view.history[0]).toMatchObject({
      role: "assistant",
      content: "已完成。",
      turnMetrics,
    });

    const completedMetrics = {
      ...turnMetrics,
      sendAcknowledgementMs: 2,
    };
    expect(replaceHistoryTurnMetrics(view, completedMetrics)).toBe(true);
    expect(view.history[0]?.turnMetrics).toEqual(completedMetrics);
    expect(
      replaceHistoryTurnMetrics(view, {
        ...completedMetrics,
        turnId: "another-turn",
      }),
    ).toBe(false);
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

  it("归约 Peri 3.6.5 主 Agent 带来源身份的真实 wire 事件", () => {
    const view = makeView();
    const mainAgentId = "019ff77a-7ad2-7dc2-a034-10568203f50b";
    const notifications: SessionUpdateEnvelope[] = [
      {
        method: "session/update",
        params: {
          sessionId: "s1",
          _peri: { sourceAgentId: mainAgentId },
          update: {
            sessionUpdate: "agent_thought_chunk",
            content: { type: "text", text: "先检查工作区。" },
            messageId: "019ff77a-7ad2-7dc2-a034-1057e68c03aa",
          },
        },
      },
      {
        method: "session/update",
        params: {
          sessionId: "s1",
          _peri: { sourceAgentId: mainAgentId },
          update: {
            sessionUpdate: "tool_call",
            toolCallId: "call-read",
            title: "Read",
            kind: "read",
            status: "in_progress",
            rawInput: { file_path: "README.md" },
          },
        },
      },
      {
        method: "session/update",
        params: {
          sessionId: "s1",
          _peri: { sourceAgentId: mainAgentId },
          update: {
            sessionUpdate: "tool_call_update",
            toolCallId: "call-read",
            title: "Read",
            status: "completed",
            rawOutput: "# KeenCode",
          },
        },
      },
      {
        method: "session/update",
        params: {
          sessionId: "s1",
          _peri: { sourceAgentId: mainAgentId },
          update: {
            sessionUpdate: "agent_message_chunk",
            content: { type: "text", text: "检查完成。" },
            messageId: "019ff77a-7ad2-7dc2-a034-1057e68c03aa",
          },
        },
      },
    ];

    for (const notification of notifications) {
      const sourceAgentId = resolveSessionUpdateSourceAgentId(
        view,
        notification.params._peri?.sourceAgentId,
      );
      reduceSessionUpdate(
        view,
        notification.params.update,
        sourceAgentId,
      );
      if (
        shouldDriveMainSessionStreaming(
          notification.params.update,
          sourceAgentId,
        )
      ) {
        view.status = "streaming";
      }
    }

    expect(view.status).toBe("streaming");
    expect(view.live_segments).toEqual([
      { kind: "thought", text: "先检查工作区。" },
      {
        kind: "tool",
        toolCallId: "call-read",
        title: "Read",
        toolKind: "Read",
        status: "completed",
        input: '{"file_path":"README.md"}',
        output: "# KeenCode",
        detail: "# KeenCode",
        streaming: false,
        isError: false,
      },
      { kind: "content", text: "检查完成。" },
    ]);
    expect(projectAcpLiveMessage(view)).toMatchObject({
      role: "assistant",
      content: "检查完成。",
      thought: "先检查工作区。",
      streaming: true,
      segments: [
        { kind: "thought", text: "先检查工作区。" },
        { kind: "tool", toolCallId: "call-read", status: "completed" },
        { kind: "content", text: "检查完成。" },
      ],
    });
  });

  it("已登记子 Agent 的 sourceAgentId 仍路由到子时间线", () => {
    const view = makeView();
    reduceAgentEvent(view, {
      type: "subagent_started",
      value: {
        agent_name: "explorer",
        instance_id: "child-agent-id",
        is_background: false,
      },
    });

    const sourceAgentId = resolveSessionUpdateSourceAgentId(
      view,
      "child-agent-id",
    );
    reduceSessionUpdate(
      view,
      {
        sessionUpdate: "agent_message_chunk",
        content: { type: "text", text: "子任务完成。" },
      },
      sourceAgentId,
    );

    expect(sourceAgentId).toBe("child-agent-id");
    expect(view.live_segments).toEqual([]);
    expect(view.subagents[0]?.segments).toEqual([
      { kind: "content", text: "子任务完成。" },
    ]);
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

  it("挂起主 Turn 后回到 ready 且后台内容不恢复主 loading", () => {
    const view = makeView();
    view.status = "streaming";
    view.retry = {
      attempt: 2,
      maxAttempts: 3,
      delayMs: 500,
      reason: "限流",
    };
    reduceAgentEvent(view, {
      type: "turn_suspended",
      value: { turn_id: "turn-1", agent_id: "main" },
    });

    reduceSessionUpdate(
      view,
      {
        sessionUpdate: "agent_message_chunk",
        content: { type: "text", text: "后台结果" },
      },
      "child-1",
    );

    expect(view.status).toBe("ready");
    expect(view.retry).toBeNull();
    expect(view.live_segments).toEqual([]);
  });

  it("持久投影系统通知和区分自动、手动压缩标记", () => {
    const view = makeView();
    reduceAgentEvent(view, {
      type: "system_notification",
      value: { text: "MCP docs 已重新连接", level: "warning" },
    });
    reduceAgentEvent(view, {
      type: "compact_completed",
      value: {
        summary: "保留关键上下文",
        files: [],
        skills: [],
        micro_cleared: 0,
        messages_json: "[]",
        strategy: "full",
        trigger: "manual",
        outcome: "completed",
      },
    });

    expect(view.history).toEqual([
      {
        role: "tool",
        content: "MCP docs 已重新连接",
        marker: "system_notification",
        systemNotificationLevel: "warning",
      },
      {
        role: "tool",
        content: "context_compact",
        marker: "context_compact",
        compactMeta: {
          trigger: "manual",
          summaryPreview: "保留关键上下文",
        },
      },
    ]);
    expect(view.compacting).toBe(false);
  });

  it("LLM 重试由事件写入，并在主 Agent 输出或终态时清理", () => {
    const view = makeView();
    reduceAgentEvent(view, {
      type: "llm_retrying",
      value: {
        attempt: 2,
        max_attempts: 4,
        delay_ms: 800,
        error: "HTTP 429",
      },
    });
    expect(view.retry).toEqual({
      attempt: 2,
      maxAttempts: 4,
      delayMs: 800,
      reason: "HTTP 429",
    });

    reduceSessionUpdate(view, {
      sessionUpdate: "agent_thought_chunk",
      content: { type: "text", text: "继续" },
    });
    expect(view.retry).toBeNull();

    reduceAgentEvent(view, {
      type: "llm_retrying",
      value: { attempt: 3, max_attempts: 4, delay_ms: 800, error: "超时" },
    });
    reduceAgentEvent(view, {
      type: "agent_execution_failed",
      value: { message: "重试耗尽" },
    });
    expect(view.retry).toBeNull();
  });

  it("新一轮用户消息清除上一轮错误状态", () => {
    const view = makeView();
    reduceAgentEvent(view, {
      type: "agent_execution_failed",
      value: { message: "LLM HTTP error (400)" },
    });
    expect(view.last_error).toEqual({
      code: "agent_execution_failed",
      message: "LLM HTTP error (400)",
    });

    reduceSessionUpdate(view, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "重试" },
    });
    expect(view.last_error).toBeNull();
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

import { describe, expect, it } from "vitest";
import {
  beginLocalSessionTurn,
  createAcpWorkspaceState,
  emptySession,
  reduceAcpTransportClosed,
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

  it("完成时不因附件引用与展示正文不同而重复用户消息", () => {
    const view = makeView();
    view.history.push({
      role: "user",
      content: "hello\n\n@image /tmp/screenshot one.png\n@/tmp/context.txt",
    });
    view.live_segments = [{ kind: "content", text: "Hi" }];

    commitLiveTurnToHistory(view, { userContent: "hello" });

    expect(view.history).toEqual([
      {
        role: "user",
        content: "hello\n\n@image /tmp/screenshot one.png\n@/tmp/context.txt",
      },
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
      reasoningTokens: 120,
      cacheReadTokens: 250,
      cacheCreationTokens: 0,
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

  it("重放空失败 Turn 时保留耗时和不完整状态", () => {
    const view = makeView();
    reduceReplayedSessionUpdate(view, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "" },
      _meta: {
        periReplay: true,
        turnStatus: "failed",
        turnDurationMs: 304_000,
        turnIncomplete: true,
        turnErrorKind: "runtime",
      },
    });
    commitLiveTurnToHistory(view);

    expect(view.history).toEqual([
      {
        role: "assistant",
        content: "",
        segments: [],
        thinkingDurationMs: 304_000,
        turnStatus: "failed",
        turnIncomplete: true,
        turnErrorKind: "runtime",
      },
    ]);
    expect(view.live_turn_metadata).toBeNull();
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
        toolKind: "read",
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
        agent_nickname: { index: 0, generation: 1 },
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

  it("归约 tool_call 与 tool_call_update，并保留真实工具标题", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call",
      toolCallId: "tc-1",
      title: "Bash",
      kind: "execute",
      rawInput: { cmd: "ls" },
    });
    expect(view.live_segments[0]).toMatchObject({
      kind: "tool",
      title: "Bash",
      toolKind: "execute",
      input: '{"cmd":"ls"}',
      isError: false,
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
      isError: false,
    });
  });

  it("归约结构化工具结果并保留截断、条目、产物、错误与命令耗时", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call",
      toolCallId: "tc-structured",
      title: "Execute",
      kind: "execute",
      status: "in_progress",
      rawInput: { command: "pnpm test" },
    });

    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-structured",
      status: "completed",
      rawOutput: {
        output: "测试失败",
        is_error: true,
        truncated: true,
        original_bytes: 8192,
        items: [
          {
            type: "command",
            command: "pnpm test",
            exit_code: 1,
            stdout: "2 passed",
            stderr: "1 failed",
            duration_ms: 275,
          },
          {
            type: "artifact",
            artifact: {
              id: "artifact-1",
              path: "/tmp/test.log",
              media_type: "text/plain",
              size_bytes: 4096,
            },
          },
        ],
        artifact: {
          id: "artifact-1",
          path: "/tmp/test.log",
          media_type: "text/plain",
          size_bytes: 4096,
        },
        extensions: [{ namespace: "peri.tool_metadata.v1", safe: true }],
      },
    });

    const tool = view.live_segments[0];
    expect(tool).toMatchObject({
      kind: "tool",
      output: "测试失败",
      detail: "测试失败",
      durationMs: 275,
      isError: true,
      structuredResult: {
        output: "测试失败",
        is_error: true,
        truncated: true,
        original_bytes: 8192,
        items: [
          {
            type: "command",
            command: "pnpm test",
            exit_code: 1,
            stdout: "2 passed",
            stderr: "1 failed",
            duration_ms: 275,
          },
          {
            type: "artifact",
            artifact: {
              id: "artifact-1",
              path: "/tmp/test.log",
              media_type: "text/plain",
              size_bytes: 4096,
            },
          },
        ],
        artifact: {
          id: "artifact-1",
          path: "/tmp/test.log",
          media_type: "text/plain",
          size_bytes: 4096,
        },
        extensions: [{ namespace: "peri.tool_metadata.v1", safe: true }],
      },
    });
  });

  it("结构化单文件与单差异结果投影路径和结果标题", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call",
      toolCallId: "tc-file",
      title: "Edit",
      kind: "edit",
      status: "in_progress",
    });
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-file",
      status: "completed",
      rawOutput: {
        output: "已修改",
        items: [{ type: "file", path: "src/App.tsx", operation: "modified" }],
      },
    });
    expect(view.live_segments[0]).toMatchObject({
      path: "src/App.tsx",
      resultTitle: "modified",
    });

    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call",
      toolCallId: "tc-diff",
      title: "Diff",
      kind: "edit",
      status: "in_progress",
    });
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-diff",
      status: "completed",
      rawOutput: {
        output: "差异已生成",
        items: [{ type: "diff", path: "src/App.tsx", patch: "@@ -1 +1 @@" }],
      },
    });
    expect(view.live_segments[1]).toMatchObject({
      path: "src/App.tsx",
      resultTitle: "diff",
    });
  });

  it("结构化结果只接受 output 字符串并过滤错误字段", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-invalid-structured",
      status: "completed",
      rawOutput: {
        output: "保留文本",
        is_error: "yes",
        truncated: "yes",
        original_bytes: "8192",
        items: [{ type: "file", path: 42, operation: "modified" }],
        artifact: { id: "missing-required-fields" },
        extensions: [null, { namespace: "valid" }],
      },
    });

    expect(view.live_segments[0]).toMatchObject({
      output: "保留文本",
      detail: "保留文本",
      isError: false,
      structuredResult: {
        output: "保留文本",
        items: [],
        extensions: [{ namespace: "valid" }],
      },
    });
    expect(view.live_segments[0]).not.toHaveProperty("truncated");
  });

  it("初始 failed tool_call 直接标记 isError，并且晚到更新不降级标题", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call",
      toolCallId: "tc-failed",
      title: "Execute command",
      kind: "execute",
      status: "failed",
      rawInput: { cmd: "npm test" },
    });
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-failed",
      title: "tool",
      kind: "execute",
      status: "failed",
      rawOutput: "exit 1",
    });

    expect(view.live_segments[0]).toMatchObject({
      title: "Execute command",
      status: "failed",
      output: "exit 1",
      detail: "exit 1",
      streaming: false,
      isError: true,
    });
  });

  it("tool_call_update 先到时保留最小占位工具段，后续 tool_call 原地补全", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-update-first",
      kind: "read",
      status: "completed",
      rawOutput: "README.md",
    });

    expect(view.live_segments).toHaveLength(1);
    expect(view.live_segments[0]).toMatchObject({
      kind: "tool",
      toolCallId: "tc-update-first",
      title: "read",
      status: "completed",
      output: "README.md",
      isError: false,
    });

    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call",
      toolCallId: "tc-update-first",
      title: "Read README.md",
      kind: "read",
      status: "in_progress",
      rawInput: { file_path: "README.md" },
    });
    expect(view.live_segments).toHaveLength(1);
    expect(view.live_segments[0]).toMatchObject({
      title: "Read README.md",
      input: '{"file_path":"README.md"}',
      output: "README.md",
      detail: "README.md",
      status: "completed",
      streaming: false,
      isError: false,
    });
  });

  it("rawOutput 缺失时读取 ACP 标准工具结果 content", () => {
    const view = makeView();
    reduceSessionUpdate(view, {
      sessionUpdate: "tool_call_update",
      toolCallId: "tc-standard-content",
      status: "failed",
      content: [
        {
          type: "content",
          content: { type: "text", text: "Tool execution failed" },
        },
      ],
    });

    expect(view.live_segments[0]).toMatchObject({
      output: "Tool execution failed",
      isError: true,
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

  it("归约 state_snapshot_meta 并保留状态栏所需的完整字段", () => {
    const view = makeView();
    reduceAgentEvent(view, {
      type: "state_snapshot_meta",
      value: {
        message_count: 12,
        total_tokens: 4321,
        current_step: 3,
        consecutive_failures: 1,
        budget_pct: 12.5,
        context_total_tokens: 128000,
      },
    });

    expect(view.state_snapshot_meta).toEqual({
      messageCount: 12,
      totalTokens: 4321,
      currentStep: 3,
      consecutiveFailures: 1,
      budgetPct: 12.5,
      contextTotalTokens: 128000,
    });
  });

  it("transport 断开时只更新状态与错误，不清理历史和当前分片", () => {
    const workspace = createAcpWorkspaceState();
    const view = emptySession("session-1");
    view.status = "streaming";
    view.history = [{ role: "user", content: "保留历史" }];
    view.live_segments = [{ kind: "content", text: "保留当前回答" }];
    view.retry = {
      attempt: 1,
      maxAttempts: 3,
      delayMs: 100,
      reason: "旧重试",
    };
    workspace.sessions[view.session_id] = view;

    expect(reduceAcpTransportClosed(workspace)).toEqual(["session-1"]);
    expect(view.status).toBe("disconnected");
    expect(view.last_error).toEqual({
      code: "agent_disconnected",
      message: "ACP transport closed",
    });
    expect(view.history).toEqual([{ role: "user", content: "保留历史" }]);
    expect(view.live_segments).toEqual([
      { kind: "content", text: "保留当前回答" },
    ]);
    expect(view.retry).toBeNull();

    // 重复 close 必须保持幂等，并继续保留断开前的消息现场。
    expect(reduceAcpTransportClosed(workspace)).toEqual(["session-1"]);
    expect(view.status).toBe("disconnected");
    expect(view.history).toEqual([{ role: "user", content: "保留历史" }]);
    expect(view.live_segments).toEqual([
      { kind: "content", text: "保留当前回答" },
    ]);
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
          files: [],
          skills: [],
          microCleared: 0,
          strategy: "full",
          outcome: "completed",
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
      value: { code: "model_request_failed", message: "重试耗尽" },
    });
    expect(view.retry).toBeNull();
  });

  it("新一轮用户消息清除上一轮错误状态", () => {
    const view = makeView();
    reduceAgentEvent(view, {
      type: "agent_execution_failed",
      value: { code: "model_http_error", message: "LLM HTTP error (400)" },
    });
    expect(view.last_error).toEqual({
      code: "model_http_error",
      message: "LLM HTTP error (400)",
    });

    reduceSessionUpdate(view, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "重试" },
    });
    expect(view.last_error).toBeNull();
  });

  it("本地发送边界立即清理上一轮错误，不等待 user_message_chunk", () => {
    const view = makeView();
    view.last_error = {
      code: "agent_execution_failed",
      message: "LLM HTTP error (502)",
    };
    view.retry = {
      attempt: 2,
      maxAttempts: 3,
      delayMs: 800,
      reason: "HTTP 502",
    };

    beginLocalSessionTurn(view, 1_787_063_943_184);

    expect(view.status).toBe("streaming");
    expect(view.last_error).toBeNull();
    expect(view.retry).toBeNull();
    expect(view.turn_started_at).toBe(1_787_063_943_184);
  });

  it("归约 subagent_started / stopped", () => {
    const view = makeView();
    reduceAgentEvent(view, {
      type: "subagent_started",
      value: {
        agent_name: "explorer",
        agent_nickname: { index: 0, generation: 1 },
        instance_id: "sa-1",
        is_background: false,
      },
    });
    expect(view.subagents).toHaveLength(1);
    expect(view.subagents[0]!.nickname).toEqual({ index: 0, generation: 1 });
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

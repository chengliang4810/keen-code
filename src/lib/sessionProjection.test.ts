import { describe, expect, it } from "vitest";
import { beginLocalSessionTurn, emptySession } from "./acp/store";
import {
  mergeAcpLiveMessage,
  mergeAcpTurnError,
  projectAcpConversation,
  projectAcpHistory,
  projectAcpSessionState,
  projectAcpSnapshot,
  projectSidebar,
} from "./sessionProjection";

describe("sessionProjection", () => {
  it("按 peri cwd 关联项目，并只从当前偏好读取展示状态", () => {
    const projection = projectSidebar(
      [
        {
          id: "session-1",
          title: "Demo",
          cwd: "/tmp/demo",
          updatedAt: "2026-08-01T00:00:00Z",
        },
      ],
      {
        "session-1": {
          archived: true,
          pinned: true,
        },
      },
      [
        {
          id: "project-1",
          name: "Demo",
          path: "/tmp/demo",
          pathOk: true,
        },
      ],
    );

    expect(projection.sessions[0]).toMatchObject({
      id: "session-1",
      projectId: "project-1",
      updatedAt: "2026-08-01T00:00:00Z",
      archived: true,
      pinned: true,
    });
    expect(projection.sessions[0]).not.toHaveProperty("scheduled");
  });

  it("只使用当前声明的 ACP Session 状态", () => {
    expect(projectAcpSessionState("streaming")).toBe("streaming");
    expect(() => projectAcpSessionState("generating")).toThrow(
      "未知 ACP Session 状态",
    );
  });

  it("将 Agent 执行失败投影为回复区错误气泡", () => {
    const view = emptySession("session-1");
    view.last_error = {
      code: "agent_execution_failed",
      message: "LLM HTTP error (400)",
    };

    const messages = mergeAcpTurnError([], view, "zh");

    expect(messages).toHaveLength(1);
    expect(messages[0]).toMatchObject({
      id: "session-1:turn-error",
      role: "assistant",
      streaming: false,
      isError: true,
    });
  });

  it("将 ACP 视图直接投影到工作台", () => {
    const view = emptySession("session-1");
    view.status = "streaming";
    view.project_path = "/tmp/demo";
    view.title = "Demo";
    view.live_segments = [
      { kind: "thought", text: "分析" },
      {
        kind: "tool",
        toolCallId: "call_1",
        title: "Read",
        toolKind: "Read",
        status: "completed",
        input: '{"path":"README.md"}',
        output: "ok",
      },
      { kind: "content", text: "结果" },
    ];

    expect(projectAcpSnapshot(view)).toMatchObject({
      sessionId: "session-1",
      state: "streaming",
      backend: "peri_acp",
      projectPath: "/tmp/demo",
    });
    expect(projectAcpSnapshot(view)).not.toHaveProperty("modelId");
    const merged = mergeAcpLiveMessage(
      [{ id: "a-pending-1", role: "assistant", content: "" }],
      view,
    );
    expect(merged).toHaveLength(1);
    expect(merged[0]).toMatchObject({
      id: "session-1:live",
      content: "结果",
      thought: "分析",
    });
  });

  it("connect/replay 尚无 live 内容时保留本地运行反馈", () => {
    const view = emptySession("session-1");
    const messages = projectAcpConversation(
      [
        { id: "u-1", role: "user", content: "你好" },
        {
          id: "a-pending-1",
          role: "assistant",
          content: "",
          streaming: true,
        },
      ],
      view,
      "zh",
      true,
    );

    expect(messages).toMatchObject([
      { id: "u-1", role: "user", content: "你好" },
      {
        id: "a-pending-1",
        role: "assistant",
        content: "",
        streaming: true,
      },
    ]);
  });

  it("新回合投影丢弃上一轮错误并保留新的 pending Assistant", () => {
    const view = emptySession("session-1");
    view.history = [{ role: "user", content: "hello" }];
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

    const messages = projectAcpConversation(
      [
        {
          id: "session-1:turn-error",
          role: "assistant",
          content: "网络或模型服务异常",
          isError: true,
        },
        { id: "u-2", role: "user", content: "第二次消息" },
        {
          id: "a-pending-2",
          role: "assistant",
          content: "",
          streaming: true,
        },
      ],
      view,
      "zh",
      true,
    );

    expect(view.status).toBe("streaming");
    expect(view.last_error).toBeNull();
    expect(view.retry).toBeNull();
    expect(view.turn_started_at).toBe(1_787_063_943_184);
    expect(messages.some((message) => message.isError)).toBe(false);
    expect(messages.map((message) => message.content)).toEqual([
      "hello",
      "第二次消息",
      "",
    ]);
    expect(messages.at(-1)).toMatchObject({
      id: "a-pending-2",
      streaming: true,
    });
  });

  it("回合终止后不再保留空的乐观 Assistant", () => {
    const view = emptySession("session-1");
    const messages = projectAcpConversation(
      [
        { id: "u-1", role: "user", content: "你好" },
        {
          id: "a-pending-1",
          role: "assistant",
          content: "",
          streaming: true,
        },
      ],
      view,
      "zh",
      false,
    );

    expect(messages).toEqual([
      { id: "u-1", role: "user", content: "你好" },
    ]);
  });

  it("恢复历史附件并隐藏用户正文中的独立路径行", () => {
    expect(
      projectAcpHistory("session-1", [
        { role: "user", content: "说明\n@/tmp/demo.png" },
      ]),
    ).toMatchObject([
      {
        id: "session-1:history:0",
        role: "user",
        content: "说明",
        attachments: [
          { path: "/tmp/demo.png", name: "demo.png", isDir: false },
        ],
      },
    ]);
  });

  it("保留系统通知和上下文压缩的时间线元数据", () => {
    expect(
      projectAcpHistory("session-1", [
        {
          role: "tool",
          content: "MCP 已断开",
          marker: "system_notification",
          systemNotificationLevel: "warning",
        },
        {
          role: "tool",
          content: "context_compact",
          marker: "context_compact",
          compactMeta: { trigger: "auto", summaryPreview: "摘要" },
        },
      ]),
    ).toMatchObject([
      {
        role: "tool",
        marker: "system_notification",
        systemNotificationLevel: "warning",
      },
      {
        role: "tool",
        marker: "context_compact",
        compactMeta: { trigger: "auto", summaryPreview: "摘要" },
      },
    ]);
  });

  it("把历史中的低延迟指标投影到 Assistant 消息", () => {
    const turnMetrics = {
      turnId: "turn-1",
      sendAcknowledgementMs: 3,
      timeToFirstSseMs: 80,
      timeToFirstVisibleTokenMs: 100,
      totalMs: 700,
      inputTokens: 600,
      cacheReadTokens: 0,
      cacheCreationTokens: null,
    };

    expect(
      projectAcpHistory("session-1", [
        { role: "assistant", content: "完成", turnMetrics },
      ]),
    ).toMatchObject([
      {
        role: "assistant",
        content: "完成",
        turnMetrics,
        streaming: false,
      },
    ]);
  });

  it("把持久化失败 Turn 投影为带耗时的空 Assistant 记录", () => {
    expect(
      projectAcpHistory("session-1", [
        {
          role: "assistant",
          content: "",
          thinkingDurationMs: 304_000,
          turnStatus: "failed",
          turnIncomplete: true,
          turnErrorKind: "runtime",
        },
      ]),
    ).toMatchObject([
      {
        role: "assistant",
        content: "",
        thinkingDurationMs: 304_000,
        turnStatus: "failed",
        turnIncomplete: true,
        turnErrorKind: "runtime",
        streaming: false,
      },
    ]);
  });

  it("不会把未知 Goal 标签解析成运行时字段并原样保留用户正文", () => {
    const content =
      '<keencode-session-goal version="1">\n旧目标\n</keencode-session-goal>\n\n继续处理';

    expect(projectAcpHistory("session-1", [{ role: "user", content }])[0])
      .toMatchObject({
        role: "user",
        content,
      });
  });
});

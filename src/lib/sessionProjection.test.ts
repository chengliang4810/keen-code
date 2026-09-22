import { describe, expect, it } from "vitest";
import { beginLocalSessionTurn, emptySession } from "./acp/store";
import type { AcpSubagentInfo } from "./acp/store";
import {
  mergeAcpLiveMessage,
  mergeAcpTurnError,
  projectAcpConversation,
  projectAcpHistory,
  projectAcpLiveMessage,
  projectAcpSessionState,
  projectAcpSnapshot,
  projectSubagentConversation,
  projectSidebar,
  projectsFromSessions,
} from "./sessionProjection";

describe("sessionProjection", () => {
  it("从 Web ACP Session cwd 派生稳定项目投影并去重路径", () => {
    const projects = projectsFromSessions([
      {
        id: "one",
        title: null,
        cwd: "D:\\Projects\\Demo\\",
        updatedAt: "",
        lastUserMessageAt: null,
      },
      {
        id: "two",
        title: null,
        cwd: "d:/projects/demo",
        updatedAt: "",
        lastUserMessageAt: null,
      },
      {
        id: "three",
        title: null,
        cwd: "D:/Projects/Other",
        updatedAt: "",
        lastUserMessageAt: null,
      },
    ]);

    expect(projects).toHaveLength(2);
    expect(projects[0]).toMatchObject({
      name: "Demo",
      path: "D:\\Projects\\Demo\\",
      pathOk: true,
    });
    expect(projects[0]?.id).toBe("web-project:d%3A%2Fprojects%2Fdemo");
    expect(projects[1]?.name).toBe("Other");
  });

  it("按 Session cwd 关联项目，并只从当前偏好读取展示状态", () => {
    const projection = projectSidebar(
      [
        {
          id: "session-1",
          title: "Demo",
          cwd: "/tmp/demo",
          updatedAt: "2026-08-01T00:00:00Z",
          lastUserMessageAt: null,
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

  it("Windows 扩展路径前缀不应使 Session 脱离所属项目", () => {
    const projection = projectSidebar(
      [
        {
          id: "session-windows",
          title: "Windows 项目",
          cwd: "\\\\?\\D:\\test\\demo",
          updatedAt: "2026-08-30T00:00:00Z",
          lastUserMessageAt: null,
        },
      ],
      {},
      [
        {
          id: "project-windows",
          name: "Demo",
          path: "D:/test/demo/",
          pathOk: true,
        },
      ],
    );

    expect(projection.sessions[0]?.projectId).toBe("project-windows");
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

  it("将结构化模型失败原文投影到错误气泡且不重复追加", () => {
    const view = emptySession("provider-session");
    const message = '模型调用失败：模型不可用：model "glm-5.3-flash" is not supported on /v1/responses; use /v1/chat/completions instead';
    view.last_error = { code: "model", message };
    {
      const messages = mergeAcpTurnError([], view, "zh");
      expect(messages[0]).toMatchObject({
        content: message,
        isError: true,
        errorBodyFormatted: true,
      });
      expect(mergeAcpTurnError(messages, view, "zh")).toHaveLength(1);
    }
  });

  it("将 ACP 视图直接投影到工作台", () => {
    const view = emptySession("session-1");
    view.status = "streaming";
    view.active_root_turn_id = "turn-1";
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
      backend: "acp",
      projectPath: "/tmp/demo",
    });
    expect(projectAcpSnapshot(view)).not.toHaveProperty("modelId");
    const merged = mergeAcpLiveMessage(
      [{ id: "a-pending-1", role: "assistant", content: "" }],
      view,
    );
    expect(merged).toHaveLength(1);
    expect(merged[0]).toMatchObject({
      id: "session-1:turn:turn-1",
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

  it("乐观用户消息只按当前 Turn 去重，不被上一轮相同正文提前吞掉", () => {
    const previous = [
      { id: "u-current", role: "user" as const, content: "相同正文" },
      {
        id: "a-pending-current",
        role: "assistant" as const,
        content: "",
        streaming: true,
      },
    ];
    const view = emptySession("session-1");
    view.active_root_turn_id = "turn-current";
    view.history = [
      {
        role: "user",
        messageId: "message-old",
        turnId: "turn-old",
        content: "相同正文",
      },
    ];

    expect(
      projectAcpConversation(previous, view, "zh", true).map(
        (message) => message.id,
      ),
    ).toEqual(["message-old", "u-current", "a-pending-current"]);

    view.history.push({
      role: "user",
      messageId: "message-current",
      turnId: "turn-current",
      content: "相同正文",
    });
    expect(
      projectAcpConversation(previous, view, "zh", true).map(
        (message) => message.id,
      ),
    ).toEqual(["message-old", "message-current", "a-pending-current"]);
  });

  it("回合结束后按历史末条用户消息去重乐观气泡，保证可编辑重发", () => {
    const previous = [
      { id: "u-1", role: "user" as const, content: "重新尝试" },
    ];
    const view = emptySession("session-1");
    view.active_root_turn_id = null;
    view.history = [
      {
        role: "user",
        messageId: "message-root",
        turnId: "turn-done",
        content: "重新尝试",
      },
    ];

    expect(
      projectAcpConversation(previous, view, "zh", false).map(
        (message) => message.id,
      ),
    ).toEqual(["message-root"]);
  });

  it("按发送语义去重位置不同的 Slash 芯片乐观消息", () => {
    const previous = [{
      id: "u-slash",
      role: "user" as const,
      content: "对未提交的代码进行代码审查 [[skill:plugin:official:code-review:code-review]]",
    }];
    const view = emptySession("session-1");
    view.active_root_turn_id = "turn-slash";
    view.history = [{
      role: "user",
      messageId: "message-slash",
      turnId: "turn-slash",
      content: "/plugin:official:code-review:code-review\n对未提交的代码进行代码审查",
    }];

    expect(projectAcpConversation(previous, view).map((message) => message.id))
      .toEqual(["message-slash"]);
  });

  it("实时与 replay 投影保留相同的用户消息标识", () => {
    const live = emptySession("session-1");
    const replay = emptySession("session-1");
    for (const view of [live, replay]) {
      view.history = [
        {
          role: "user",
          messageId: "message-same",
          turnId: "turn-1",
          content: "重复正文",
        },
        {
          role: "assistant",
          messageId: "message-assistant",
          turnId: "turn-1",
          content: "完成",
        },
      ];
    }

    expect(projectAcpHistory("session-1", live.history)).toEqual(
      projectAcpHistory("session-1", replay.history),
    );
    expect(projectAcpHistory("session-1", live.history).map(
      (message) => message.id,
    )).toEqual(["message-same", "message-assistant"]);
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

  it("不把用户原文中的绝对图片路径推断为附件", () => {
    expect(
      projectAcpHistory("session-1", [
        {
          role: "user",
          content: "这是原文：/tmp/not-an-attachment.png",
        },
      ]),
    ).toMatchObject([
      {
        role: "user",
        content: "这是原文：/tmp/not-an-attachment.png",
        attachments: undefined,
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
      timeToFirstTokenMs: 100,
      timeToFirstVisibleTokenMs: 100,
      totalMs: 700,
      inputTokens: 600,
              outputTokens: null,
              totalTokens: null,
      reasoningTokens: 50,
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

  it("把持久化的 Assistant 模型关联到对应用户回合", () => {
    expect(
      projectAcpHistory("session-1", [
        { role: "user", content: "第一条" },
        { role: "assistant", content: "完成", model: "gpt-5.6-luna" },
      ]),
    ).toMatchObject([
      { role: "user", content: "第一条", model: "gpt-5.6-luna" },
      { role: "assistant", content: "完成", model: "gpt-5.6-luna" },
    ]);
  });

  it("子 Agent 每轮使用固化结果，失败优先展示错误且保留完整 segments", () => {
    const agent: AcpSubagentInfo = {
      agent_id: "child-1",
      agent_name: "review",
      task_title: "检查",
      prompt: "检查项目",
      nickname: null,
      status: "failed",
      is_background: false,
      started_at: 1,
      stopped_at: 4,
      result: "最终错误",
      segments: [
        { kind: "content", text: "前置正文" },
        { kind: "tool", toolCallId: "read", title: "Read", status: "completed" },
        { kind: "content", text: "失败前的正文" },
      ],
      turns: [{
        metrics: {
          turnId: "turn-1",
          startedAtMs: 1,
          deliveryInterrupted: false,
          sendAcknowledgedAtMs: null,
          firstSseAtMs: null,
          firstTokenAtMs: null,
          firstVisibleTokenAtMs: null,
          completedAtMs: 4,
          usageObservations: [],
        },
        segmentStart: 0,
        segmentEnd: 3,
        status: "failed",
        result: "不应优先显示",
        error: "稳定失败说明",
      }],
    };
    const [message] = projectSubagentConversation(agent).filter((item) => item.role === "assistant");
    expect(message).toMatchObject({
      content: "稳定失败说明",
      segments: agent.segments,
      streaming: false,
    });
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

  it("同一历史数组重复投影复用缓存，history 增长或替换后重新投影", () => {
    const history = [
      { role: "user" as const, content: "问题" },
      { role: "assistant" as const, content: "回答", model: "m-1" },
    ];
    const first = projectAcpHistory("session-1", history);
    expect(projectAcpHistory("session-1", history)).toBe(first);

    // push 新消息：数组身份不变但长度变化，必须失效重投影。
    history.push({ role: "user" as const, content: "追问" });
    const grown = projectAcpHistory("session-1", history);
    expect(grown).not.toBe(first);
    expect(grown.length).toBe(3);

    // 整体替换（历史页回填路径）是新数组身份，同样重新投影。
    const replaced = [...history];
    expect(projectAcpHistory("session-1", replaced)).not.toBe(grown);
  });

  it("原地修改既有历史消息时按修订号失效缓存", () => {
    const view = emptySession("session-1");
    view.history = [
      { role: "assistant" as const, turnId: "turn-1", content: "分片一" },
    ];
    view.history_revision = 0;
    const first = projectAcpHistory("session-1", view.history, view.history_revision);
    expect(
      projectAcpHistory("session-1", view.history, view.history_revision),
    ).toBe(first);

    // 分片追加/指标补写等原地修改：数组身份与长度都不变，仅修订号变化。
    view.history[0]!.content += "分片二";
    view.history_revision += 1;
    const reprojected = projectAcpHistory(
      "session-1",
      view.history,
      view.history_revision,
    );
    expect(reprojected).not.toBe(first);
    expect(reprojected[0]?.content).toBe("分片一分片二");
  });

  it("流式正文缓存跟随尾段追加、阶段追加和非追加回退", () => {
    const view = emptySession("session-1");
    view.status = "streaming";
    view.active_root_turn_id = "turn-1";
    view.live_segments = [{ kind: "content", text: "第一段" }];

    expect(projectAcpLiveMessage(view)?.content).toBe("第一段");
    const firstContent = view.live_segments[0];
    if (firstContent?.kind !== "content") throw new Error("测试正文段缺失");
    firstContent.text += "追加";
    expect(projectAcpLiveMessage(view)?.content).toBe("第一段追加");

    view.live_segments.push({ kind: "thought", text: "补充思考" });
    const interleaved = projectAcpLiveMessage(view);
    expect(interleaved?.content).toBe("第一段追加");
    expect(interleaved?.thought).toBe("补充思考");

    // 外部恢复/测试数据可能原地替换文本，长度不再是 append-only；
    // 此时必须回退全文重建，不能沿用旧正文。
    const lastThought = view.live_segments[1];
    if (lastThought?.kind !== "thought") throw new Error("测试思考段缺失");
    lastThought.text = "替换";
    expect(projectAcpLiveMessage(view)?.thought).toBe("替换");
  });
});

import { describe, expect, it } from "vitest";
import { parseSessionUpdateDeliveryEnvelope } from "./events";
import type {
  KeenCodeEvent,
  KeenCodeEventEnvelope,
  SessionUpdate,
  SessionUpdateDeliveryEnvelope,
} from "./events";
import {
  beginLocalSessionTurn,
  beginSessionRecovery,
  completeSessionRecovery,
  emptySession,
  failSessionRecovery,
  reduceDeliveryEnvelope,
  reduceGoalSnapshot,
  reduceReplayResult,
  type AcpSessionView,
} from "./store";
import { fileChangeUri } from "./fileChanges";

/** 构造指定水位的根或子 Agent 标准更新。 */
function updateDelivery(
  sequence: number,
  update: SessionUpdate,
  sourceAgentId = "root",
  turnId = "turn-1",
): SessionUpdateDeliveryEnvelope {
  return {
    schemaVersion: 1,
    sessionId: "session-1",
    turnId,
    sourceAgentId,
    deliverySequence: sequence,
    occurredAtMs: 1_000 + sequence,
    update,
  };
}

/** 将测试信封经过 JSON 往返，模拟冷回放收到的标准 ACP 载荷。 */
function parsedUpdateDelivery(
  sequence: number,
  update: SessionUpdate,
): SessionUpdateDeliveryEnvelope {
  const parsed = parseSessionUpdateDeliveryEnvelope(
    JSON.parse(JSON.stringify(updateDelivery(sequence, update))),
  );
  if (!parsed) throw new Error("测试更新信封未通过 ACP 校验");
  return parsed;
}

/** 构造指定水位的生命周期事件。 */
function eventDelivery(
  sequence: number,
  event: KeenCodeEvent,
  options: {
    sourceAgentId?: string;
    turnId?: string;
    journalSequence?: number;
    sessionScoped?: boolean;
  } = {},
): KeenCodeEventEnvelope {
  const scoped = options.sessionScoped === true;
  return {
    schemaVersion: 1,
    sessionId: "session-1",
    ...(!scoped
      ? {
          turnId: options.turnId ?? "turn-1",
          sourceAgentId: options.sourceAgentId ?? "root",
        }
      : {}),
    ...(options.journalSequence === undefined
      ? {}
      : { journalSequence: options.journalSequence }),
    deliverySequence: sequence,
    occurredAtMs: 1_000 + sequence,
    event,
  };
}

/** 应用信封并断言经过唯一 Reducer。 */
function apply(
  view: AcpSessionView,
  envelope: SessionUpdateDeliveryEnvelope | KeenCodeEventEnvelope,
): void {
  const result = reduceDeliveryEnvelope(view, envelope);
  if (result.status !== "applied") {
    throw new Error(`delivery was not applied: ${result.status}`);
  }
}

/** 构造覆盖正文、思考、工具和终态的一轮事件。 */
function completeTurn(view: AcpSessionView): void {
  apply(view, eventDelivery(1, {
    type: "turn_started",
    rootTurnId: "turn-1",
  }, { journalSequence: 1 }));
  apply(view, updateDelivery(2, {
    sessionUpdate: "user_message_chunk",
    content: { type: "text", text: "检查项目" },
    _meta: { "keencode/messageId": "message-turn-1" },
  }));
  apply(view, updateDelivery(3, {
    sessionUpdate: "agent_thought_chunk",
    content: { type: "text", text: "先读取" },
  }));
  apply(view, updateDelivery(4, {
    sessionUpdate: "tool_call",
    toolCallId: "tool-1",
    title: "Read src/App.tsx",
    kind: "read",
    status: "in_progress",
    rawInput: { path: "src/App.tsx" },
  }));
  apply(view, updateDelivery(5, {
    sessionUpdate: "tool_call_update",
    toolCallId: "tool-1",
    status: "completed",
    rawOutput: "读取完成",
  }));
  apply(view, updateDelivery(6, {
    sessionUpdate: "agent_message_chunk",
    content: { type: "text", text: "已完成" },
  }));
  apply(view, eventDelivery(7, { type: "turn_completed" }, {
    journalSequence: 2,
  }));
}

describe("Acp delivery sequence", () => {
  it("原生取消从唯一完整结果提取正文，迟到调用不能回退终态", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, { type: "turn_started", rootTurnId: "turn-1" }));
    apply(view, updateDelivery(2, {
      sessionUpdate: "tool_call_update", toolCallId: "cancel-command", status: "failed",
      _meta: { "keencode/toolOutcome": "cancelled" },
      rawOutput: { toolCallId: "cancel-command", isError: true, content: [{ type: "text", text: "已取消" }] },
    }));
    apply(view, updateDelivery(3, {
      sessionUpdate: "tool_call", toolCallId: "cancel-command", title: "Git", status: "pending",
      rawInput: { args: ["status"] },
    }));
    const segment = view.live_segments.find(item => item.kind === "tool");
    expect(segment).toMatchObject({ status: "failed", completionStatus: "cancelled", output: "已取消", streaming: false });
    expect(segment?.detail).toBe("已取消");
  });

  it("原生多段文本与标准 Diff 共存，保留换行、空文本和转义内容", () => {
    const view = emptySession("session-1");
    apply(view, updateDelivery(1, {
      sessionUpdate: "tool_call_update", toolCallId: "native-mixed", status: "completed",
      rawOutput: { toolCallId: "native-mixed", isError: false, content: [
        { type: "text", text: "第一行\r\n\\正文" }, { type: "text", text: "" },
        { type: "text", text: "末行" },
      ] },
      content: [{ type: "diff", path: "result.txt", oldText: "before", newText: "after" }],
    }));
    expect(view.live_segments[0]).toMatchObject({ output: "第一行\r\n\\正文\n\n末行",
      detail: "第一行\r\n\\正文\n\n末行", fileChanges: [{ path: "result.txt" }] });
  });

  it("不把其他工具身份或畸形结果误认作当前原生正文", () => {
    for (const rawOutput of [
      { toolCallId: "another-tool", isError: false, content: [{ type: "text", text: "错误身份" }] },
      { toolCallId: "native-invalid", isError: false, content: [{ type: "text", text: 42 }] },
    ]) {
      const view = emptySession("session-1");
      apply(view, updateDelivery(1, { sessionUpdate: "tool_call_update", toolCallId: "native-invalid",
        status: "completed", rawOutput }));
      expect(view.live_segments[0]).toMatchObject({ output: JSON.stringify(rawOutput) });
    }
  });

  it("原生文本旁的非文本引用保留在详情，不因提取正文而丢失", () => {
    const reference = { type: "artifact", artifact: { artifactId: "result-artifact", sizeBytes: 100000 } };
    const view = emptySession("session-1");
    apply(view, updateDelivery(1, { sessionUpdate: "tool_call_update", toolCallId: "native-artifact",
      status: "completed", rawOutput: { toolCallId: "native-artifact", isError: false,
        content: [{ type: "text", text: "结果已落盘" }, reference] } }));
    expect(view.live_segments[0]).toMatchObject({ output: `结果已落盘\n${JSON.stringify(reference)}` });
  });

  it("冷回放先收到取消结果更新时保留终态并以标准 content 优先于 rawOutput", () => {
    const view = emptySession("session-1");
    const resultFirst = parsedUpdateDelivery(1, {
      sessionUpdate: "tool_call_update",
      toolCallId: "cancel-cold",
      status: "failed",
      _meta: { "keencode/toolOutcome": "cancelled" },
      content: [
        {
          type: "content",
          content: { type: "text", text: "标准取消结果" },
        },
      ],
      rawOutput: "rawOutput 不应覆盖标准内容",
    });
    apply(view, resultFirst);

    expect(view.live_segments).toHaveLength(1);
    expect(view.live_segments[0]).toMatchObject({
      kind: "tool",
      toolCallId: "cancel-cold",
      status: "failed",
      completionStatus: "cancelled",
      output: "标准取消结果",
      detail: "标准取消结果",
      streaming: false,
      isError: true,
    });

    // 后到的请求只补充标题和输入，不能把已提交的取消结果改回运行态。
    apply(view, parsedUpdateDelivery(2, {
      sessionUpdate: "tool_call",
      toolCallId: "cancel-cold",
      title: "Git",
      kind: "execute",
      status: "pending",
      rawInput: { args: ["status"] },
    }));

    expect(view.live_segments).toHaveLength(1);
    expect(view.live_segments[0]).toMatchObject({
      toolCallId: "cancel-cold",
      title: "Git",
      input: '{"args":["status"]}',
      status: "failed",
      completionStatus: "cancelled",
      output: "标准取消结果",
      detail: "标准取消结果",
      streaming: false,
    });
  });

  it("标准 Diff 经真实信封保留精确快照，终态重复调用和纯状态更新不会覆盖结果", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, { type: "turn_started", rootTurnId: "turn-1" }, {
      journalSequence: 1,
    }));
    const fileChanges = [
      { path: "new.txt", oldText: null, newText: "\uFEFF正文\r\n" },
      { path: "empty.txt", oldText: "", newText: "" },
    ];
    const parsed = parseSessionUpdateDeliveryEnvelope(JSON.parse(JSON.stringify(updateDelivery(2, {
      sessionUpdate: "tool_call_update",
      toolCallId: "diff-call",
      status: "completed",
      content: fileChanges.map((change) => ({ type: "diff", ...change })),
    }))));
    expect(parsed).not.toBeNull();
    apply(view, parsed!);
    apply(view, updateDelivery(3, {
      sessionUpdate: "tool_call", toolCallId: "diff-call", title: "Write",
      status: "in_progress", content: [],
    }));
    apply(view, updateDelivery(4, {
      sessionUpdate: "tool_call_update", toolCallId: "diff-call", status: "completed",
    }));
    expect(view.live_segments).toHaveLength(1);
    expect(view.live_segments[0]).toMatchObject({
      kind: "tool", status: "completed", fileChanges,
    });
    apply(view, updateDelivery(5, {
      sessionUpdate: "tool_call_update", toolCallId: "diff-call", content: [],
    }));
    expect(view.live_segments[0]).toMatchObject({ fileChanges: [] });
  });

  it("文件快照仅来自标准 Diff，rawOutput 不产生快照且 diff 更新不清空已有文本", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, { type: "turn_started", rootTurnId: "turn-1" }, {
      journalSequence: 1,
    }));
    apply(view, updateDelivery(2, {
      sessionUpdate: "tool_call", toolCallId: "raw-diff", title: "Edit",
    }));
    apply(view, updateDelivery(3, {
      sessionUpdate: "tool_call_update", toolCallId: "raw-diff", status: "completed",
      rawOutput: { type: "diff", path: "file.txt", oldText: "old", newText: "new" },
    }));
    expect(view.live_segments[0]).not.toHaveProperty("fileChanges");
    const originalOutput = view.live_segments[0].kind === "tool" ? view.live_segments[0].output : undefined;
    apply(view, updateDelivery(4, {
      sessionUpdate: "tool_call_update", toolCallId: "raw-diff",
      content: [{ type: "diff", path: "file.txt", oldText: "old", newText: "new" }],
    }));
    expect(view.live_segments[0]).toMatchObject({
      output: originalOutput,
      fileChanges: [{ path: "file.txt", oldText: "old", newText: "new" }],
    });
  });

  it("同 Session resource_link 生成引用、content 省略保留、空数组清空且跨 Session 丢弃", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, { type: "turn_started", rootTurnId: "turn-1" }, {
      journalSequence: 1,
    }));
    const emptySha256 =
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const reference = {
      sessionId: "session-1",
      requestId: "write-1",
      path: "C:/workspace/empty.txt",
      before: null,
      after: { sizeBytes: 0, sha256: emptySha256 },
      applied: true,
    };
    const resource = {
      type: "content" as const,
      content: {
        type: "resource_link" as const,
        name: "empty.txt",
        uri: fileChangeUri(reference.sessionId, reference.requestId),
        _meta: { "keencode/fileChange": reference },
      },
    };
    const parsed = parseSessionUpdateDeliveryEnvelope(JSON.parse(JSON.stringify(
      updateDelivery(2, {
        sessionUpdate: "tool_call_update",
        toolCallId: "write-1",
        status: "completed",
        content: [resource],
      }),
    )));
    expect(parsed).not.toBeNull();
    apply(view, parsed!);
    expect(view.live_segments[0]).toMatchObject({
      fileChanges: [{ path: reference.path, reference }],
    });

    // content 省略时，重复终态更新不能丢弃已有的权威引用。
    apply(view, updateDelivery(3, {
      sessionUpdate: "tool_call_update",
      toolCallId: "write-1",
      status: "completed",
    }));
    expect(view.live_segments[0]).toMatchObject({
      fileChanges: [{ path: reference.path, reference }],
    });

    // 明确的空数组才清除本次工具的文件变更。
    apply(view, updateDelivery(4, {
      sessionUpdate: "tool_call_update",
      toolCallId: "write-1",
      content: [],
    }));
    expect(view.live_segments[0]).toMatchObject({ fileChanges: [] });

    // 跨 Session 引用即使 URI 和 descriptor 自洽，也不能投影成当前 Session Diff。
    const crossSession = {
      ...resource,
      content: {
        ...resource.content,
        uri: fileChangeUri("session-2", "write-2"),
        _meta: {
          "keencode/fileChange": {
            ...reference,
            sessionId: "session-2",
            requestId: "write-2",
          },
        },
      },
    };
    const crossParsed = parseSessionUpdateDeliveryEnvelope(JSON.parse(JSON.stringify(
      updateDelivery(5, {
        sessionUpdate: "tool_call_update",
        toolCallId: "cross-write",
        content: [crossSession],
      }),
    )));
    expect(crossParsed).not.toBeNull();
    apply(view, crossParsed!);
    expect(view.live_segments[1]).toMatchObject({ fileChanges: [] });
  });

  it("首次投递必须从 1 开始，缺口后立即冻结", () => {
    const view = emptySession("session-1");
    expect(reduceDeliveryEnvelope(view, updateDelivery(2, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "越过首条" },
    }))).toEqual({
      status: "gap",
      expectedSequence: 1,
      receivedSequence: 2,
    });
    expect(view.delivery).toEqual({
      lastSequence: null,
      frozen: true,
      expectedSequence: 1,
      receivedSequence: 2,
    });
    expect(reduceDeliveryEnvelope(view, updateDelivery(1, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "迟到首条" },
    }))).toEqual({ status: "frozen" });
    expect(view.live_segments).toEqual([]);
  });

  it("标准更新和扩展事件共享同一水位，重复投递保持幂等", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, {
      type: "turn_started",
      rootTurnId: "turn-1",
    }, { journalSequence: 1 }));
    apply(view, updateDelivery(2, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "A" },
    }));
    expect(reduceDeliveryEnvelope(view, updateDelivery(2, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "A" },
    }))).toEqual({ status: "duplicate" });
    apply(view, eventDelivery(3, {
      type: "model_first_stream_observed",
    }));

    expect(view.delivery.lastSequence).toBe(3);
    expect(view.live_segments).toEqual([{ kind: "content", text: "A" }]);
  });

  it("Session 标识不匹配时冻结而不污染投影", () => {
    const view = emptySession("session-1");
    const wrong = { ...updateDelivery(1, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "错误会话" },
    }), sessionId: "session-2" };
    expect(reduceDeliveryEnvelope(view, wrong)).toEqual({ status: "frozen" });
    expect(view.delivery.frozen).toBe(true);
    expect(view.live_segments).toEqual([]);
  });
});

describe("Acp realtime and replay equivalence", () => {
  it("实时与 replay 使用同一事件序列得到完全相同投影", () => {
    const live = emptySession("session-1");
    const replay = emptySession("session-1");
    completeTurn(live);
    completeTurn(replay);

    expect(replay).toEqual(live);
    expect(live.history).toHaveLength(2);
    expect(live.history[0]).toMatchObject({
      role: "user",
      content: "检查项目",
      messageId: "message-turn-1",
      turnId: "turn-1",
    });
    expect(live.history[1]).toMatchObject({
      role: "assistant",
      content: "已完成",
      thought: "先读取",
      turnId: "turn-1",
      turnStatus: "completed",
      turnIncomplete: false,
    });
    expect(live.history[1]?.segments?.map((segment) => segment.kind)).toEqual([
      "thought",
      "tool",
      "content",
    ]);
  });

  it("只合并同一稳定消息标识的多分片，不合并相同正文的不同消息", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, {
      type: "turn_started",
      rootTurnId: "turn-1",
    }, { journalSequence: 1 }));
    apply(view, updateDelivery(2, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "重复正文" },
      _meta: { "keencode/messageId": "message-1" },
    }));
    apply(view, updateDelivery(3, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "-续写" },
      _meta: { "keencode/messageId": "message-1" },
    }));
    apply(view, updateDelivery(4, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "重复正文" },
      _meta: { "keencode/messageId": "message-2" },
    }));

    expect(view.history).toMatchObject([
      {
        role: "user",
        messageId: "message-1",
        turnId: "turn-1",
        content: "重复正文-续写",
      },
      {
        role: "user",
        messageId: "message-2",
        turnId: "turn-1",
        content: "重复正文",
      },
    ]);
    expect(view.history).toHaveLength(2);
  });

  it("上一轮没有 Assistant 时，新 Turn 的用户消息不会并入旧消息", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, {
      type: "turn_started",
      rootTurnId: "turn-old",
    }, { turnId: "turn-old", journalSequence: 1 }));
    apply(view, updateDelivery(2, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "相同正文" },
      _meta: { "keencode/messageId": "message-old" },
    }, "root", "turn-old"));

    // 模拟上一轮尚未产生 Assistant 内容就进入下一轮。
    apply(view, eventDelivery(3, {
      type: "turn_started",
      rootTurnId: "turn-new",
    }, { turnId: "turn-new", journalSequence: 2 }));
    apply(view, updateDelivery(4, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "相同正文" },
      _meta: { "keencode/messageId": "message-new" },
    }, "root", "turn-new"));

    expect(view.history).toMatchObject([
      { role: "user", messageId: "message-old", turnId: "turn-old" },
      { role: "user", messageId: "message-new", turnId: "turn-new" },
    ]);
    expect(view.history).toHaveLength(2);
  });

  it("没有消息元数据时只在同一 Turn 内合并分片", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, {
      type: "turn_started",
      rootTurnId: "turn-1",
    }, { turnId: "turn-1", journalSequence: 1 }));
    apply(view, updateDelivery(2, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "无标识" },
    }, "root", "turn-1"));
    apply(view, updateDelivery(3, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "-续写" },
    }, "root", "turn-1"));
    apply(view, eventDelivery(4, {
      type: "turn_started",
      rootTurnId: "turn-2",
    }, { turnId: "turn-2", journalSequence: 2 }));
    apply(view, updateDelivery(5, {
      sessionUpdate: "user_message_chunk",
      content: { type: "text", text: "无标识" },
    }, "root", "turn-2"));

    expect(view.history).toMatchObject([
      { role: "user", turnId: "turn-1", content: "无标识-续写" },
      { role: "user", turnId: "turn-2", content: "无标识" },
    ]);
    expect(view.history).toHaveLength(2);
  });

  it("根 Turn 成功结束后仍保留最后一条权威 Todo 投影", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, {
      type: "turn_started",
      rootTurnId: "turn-1",
    }, { journalSequence: 1 }));
    apply(view, updateDelivery(2, {
      sessionUpdate: "plan",
      entries: [{
        content: "继续验证 Provider",
        priority: "high",
        status: "in_progress",
      }],
    }));
    apply(view, updateDelivery(3, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "本轮完成，Todo 留待下一轮。" },
    }));
    apply(view, eventDelivery(4, { type: "turn_completed" }, {
      journalSequence: 2,
    }));

    expect(view.todos).toEqual({
      revision: 1,
      items: [{ content: "继续验证 Provider", status: "in_progress" }],
    });

    apply(view, {
      schemaVersion: 1,
      sessionId: "session-1",
      deliverySequence: 5,
      occurredAtMs: 1_005,
      update: { sessionUpdate: "plan", entries: [] },
    });
    expect(view.todos).toEqual({ revision: 2, items: [] });
  });

  it("Todo 投影使用 Runtime 提供的 revision，而不是本地到达次数", () => {
    const view = emptySession("session-1");
    apply(view, updateDelivery(1, {
      sessionUpdate: "plan",
      entries: [{
        content: "首次计划",
        priority: "medium",
        status: "in_progress",
      }],
      _meta: { _keencode: { todoRevision: 9 } },
    }));
    apply(view, updateDelivery(2, {
      sessionUpdate: "plan",
      entries: [],
      _meta: { _keencode: { todoRevision: 10 } },
    }));

    expect(view.todos).toEqual({ revision: 10, items: [] });
  });

  it("终态后的迟到更新只推进共享水位，不改写已提交结果", () => {
    const view = emptySession("session-1");
    completeTurn(view);
    const before = structuredClone(view.history);
    const result = reduceDeliveryEnvelope(view, updateDelivery(8, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "迟到内容" },
    }));

    expect(result).toEqual({
      status: "applied",
      ignoredTerminalUpdate: true,
    });
    expect(view.delivery.lastSequence).toBe(8);
    expect(view.history).toEqual(before);
    expect(view.live_segments).toEqual([]);
  });
});

describe("Acp subagent projection", () => {
  it("子 Agent 流独立归约且不会驱动根 Agent 正文", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, {
      type: "agent_spawned",
      agentId: "child-1",
      parentAgentId: "root",
      agentPath: "/root/review",
      task: "核对子任务",
      parentTurnId: "turn-1",
      rootTurnId: "turn-1",
    }, { journalSequence: 1 }));
    const childResult = reduceDeliveryEnvelope(view, updateDelivery(2, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "子任务完成" },
    }, "child-1", "child-turn-1"));
    expect(childResult).toEqual({
      status: "applied",
      childAgentId: "child-1",
      ignoredTerminalUpdate: false,
    });
    apply(view, eventDelivery(3, { type: "turn_completed" }, {
      sourceAgentId: "child-1",
      turnId: "child-turn-1",
      journalSequence: 2,
    }));

    expect(view.live_segments).toEqual([]);
    expect(view.subagents).toEqual([
      expect.objectContaining({
        agent_id: "child-1",
        agent_name: "review",
        status: "done",
        result: null,
        segments: [{ kind: "content", text: "子任务完成" }],
      }),
    ]);
  });

  it("子回合取消独立投影为中断，不依赖后续生命周期事件纠正", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, {
      type: "agent_spawned",
      agentId: "child-1",
      parentAgentId: "root",
      agentPath: "/root/review",
      task: "核对取消",
      parentTurnId: "turn-1",
      rootTurnId: "turn-1",
    }, { journalSequence: 1 }));
    apply(view, eventDelivery(2, { type: "turn_cancelled" }, {
      sourceAgentId: "child-1",
      turnId: "child-turn-1",
      journalSequence: 2,
    }));
    expect(view.subagents[0]?.status).toBe("interrupted");
    expect(view.subagents[0]?.stopped_at).not.toBeNull();
  });

  it("中断生命周期保持中断，后台取消不会覆盖为失败", () => {
    const view = emptySession("session-1");
    apply(view, eventDelivery(1, {
      type: "agent_spawned",
      agentId: "child-1",
      parentAgentId: "root",
      agentPath: "/root/review",
      task: "核对子任务",
      parentTurnId: "turn-1",
      rootTurnId: "turn-1",
    }, { journalSequence: 1 }));
    apply(view, eventDelivery(2, {
      type: "agent_status_changed",
      agentId: "child-1",
      status: "interrupted",
    }, {
      sourceAgentId: "child-1",
      turnId: "child-turn-1",
      journalSequence: 2,
    }));
    expect(view.subagents[0]?.status).toBe("interrupted");

    apply(view, eventDelivery(3, {
      type: "background_task_completed",
      taskId: "task-1",
      taskKind: "agent",
      agentId: "child-1",
      status: "cancelled",
      durationMs: 100,
    }, { sessionScoped: true }));
    expect(view.subagents[0]?.status).toBe("interrupted");
  });
});

describe("Acp recovery and control projections", () => {
  it("恢复重置不可信投影并保留项目绑定", () => {
    const view = emptySession("session-1");
    view.project_path = "D:/projects/demo";
    completeTurn(view);

    beginSessionRecovery(view);
    expect(view.project_path).toBe("D:/projects/demo");
    expect(view.status).toBe("connecting");
    expect(view.replay.restoring).toBe(true);
    expect(view.delivery.lastSequence).toBeNull();
    expect(view.history).toEqual([]);
    completeSessionRecovery(view);
    expect(view.status).toBe("ready");
    expect(view.replay.restoring).toBe(false);
  });

  it("恢复等待新世代序号一时忽略仍在途的旧世代 live 投递", () => {
    const view = emptySession("session-1");
    completeTurn(view);
    beginSessionRecovery(view);

    expect(reduceDeliveryEnvelope(view, updateDelivery(8, {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "旧世代迟到增量" },
    }))).toEqual({ status: "stale_generation" });
    expect(view.delivery.lastSequence).toBeNull();
    expect(view.delivery.frozen).toBe(false);
    expect(view.live_segments).toEqual([]);

    apply(view, eventDelivery(1, {
      type: "turn_started",
      rootTurnId: "turn-replayed",
    }, { turnId: "turn-replayed", journalSequence: 1 }));
    expect(view.delivery.lastSequence).toBe(1);
    expect(view.delivery.frozen).toBe(false);
  });

  it("恢复失败保持冻结并记录稳定错误", () => {
    const view = emptySession("session-1");
    beginSessionRecovery(view);
    failSessionRecovery(view, "replay unavailable");
    expect(view.status).toBe("ready");
    expect(view.delivery.frozen).toBe(true);
    expect(view.last_error).toEqual({
      code: "session_recovery_failed",
      message: "replay unavailable",
    });
  });

  it("Replay 控制响应只推进 Journal 水位与分页状态", () => {
    const view = emptySession("session-1");
    reduceReplayResult(view, {
      sessionId: "session-1",
      startAfter: 0,
      nextAfter: 25,
      throughJournalSequence: 40,
      throughDeliverySequence: 25,
      replayedEvents: 25,
      hasMore: true,
    });
    expect(view.replay).toEqual({
      loaded: false,
      throughDeliverySequence: 25,
      after: 25,
      throughJournalSequence: 40,
      hasMore: true,
      restoring: false,
    });
    reduceReplayResult(view, {
      sessionId: "session-1",
      startAfter: 25,
      nextAfter: 0,
      throughJournalSequence: 0,
      throughDeliverySequence: 25,
      replayedEvents: 0,
      hasMore: false,
    });
    expect(view.replay.after).toBeNull();
  });

  it("Goal 查询结果使用 camelCase DTO 全量替换", () => {
    const view = emptySession("session-1");
    const goal = {
      id: "goal-1",
      title: "完成重写",
      scope: "project" as const,
      status: "active" as const,
      objective: "完成 Runtime 重写",
      tokensUsed: 10,
      timeUsedSeconds: 2,
      createdAtMs: 1,
      updatedAtMs: 2,
    };
    reduceGoalSnapshot(view, 3, goal);
    expect(view.goal).toEqual({ revision: 3, goal });
  });

  it("本地发起新轮次只清理上一轮瞬时状态", () => {
    const view = emptySession("session-1");
    view.last_error = { code: "model", message: "old" };
    view.retry = { attempt: 1, maxAttempts: 3, delayMs: 100, reason: "old" };
    beginLocalSessionTurn(view, 500);
    expect(view).toMatchObject({
      status: "streaming",
      last_error: null,
      retry: null,
      turn_started_at: 500,
      live_turn_metadata: null,
    });
  });
});

import { describe, expect, it } from "vitest";
import {
  isAuthoritativeKeenCodeEvent,
  isSessionScopedKeenCodeEvent,
  isSessionScopedUpdate,
  isTerminalKeenCodeEvent,
  mergeSessionTextUpdates,
  parseAcpJsonRpcClientRequest,
  parseAcpTauriDelivery,
  parseKeenCodeEventEnvelope,
  parseSessionUpdateDeliveryEnvelope,
  shouldDriveMainSessionStreaming,
  type KeenCodeEvent,
  type SessionUpdate,
} from "./events";
import { fileChangeUri } from "./fileChanges";

describe("精确工具终态元数据", () => {
  it.each(["cancelled", "failed", "side_effect_unknown"])("保留标准failed与%s", (outcome) => {
    expect(parseSessionUpdateDeliveryEnvelope(updateEnvelope({
      sessionUpdate: "tool_call_update", toolCallId: "tool-1", status: "failed",
      _meta: { "keencode/toolOutcome": outcome },
    }))).not.toBeNull();
  });
  it.each(["running", "cancel", null, 12])("拒绝未知自有终态%j", (outcome) => {
    expect(parseSessionUpdateDeliveryEnvelope(updateEnvelope({
      sessionUpdate: "tool_call_update", toolCallId: "tool-1", status: "failed",
      _meta: { "keencode/toolOutcome": outcome },
    }))).toBeNull();
  });
  it("拒绝cancelled与标准completed矛盾", () => {
    expect(parseSessionUpdateDeliveryEnvelope(updateEnvelope({
      sessionUpdate: "tool_call_update", toolCallId: "tool-1", status: "completed",
      _meta: { "keencode/toolOutcome": "cancelled" },
    }))).toBeNull();
  });
});

/** 构造根 Agent 的标准更新信封。 */
function updateEnvelope(
  update: SessionUpdate,
): Record<string, unknown> & { update: SessionUpdate } {
  return {
    schemaVersion: 1,
    sessionId: "session-1",
    turnId: "turn-1",
    sourceAgentId: "root",
    deliverySequence: 1,
    occurredAtMs: 1_000,
    update,
  };
}

/** 构造需要进入 Session Journal 的生命周期信封。 */
function eventEnvelope(event: KeenCodeEvent): Record<string, unknown> {
  return {
    schemaVersion: 1,
    sessionId: "session-1",
    turnId: "turn-1",
    sourceAgentId: "root",
    journalSequence: 1,
    deliverySequence: 1,
    occurredAtMs: 1_000,
    event,
  };
}

/** 构造不绑定 Turn 和 Agent 的 Session 级事件信封。 */
function sessionEventEnvelope(event: KeenCodeEvent): Record<string, unknown> {
  return {
    schemaVersion: 1,
    sessionId: "session-1",
    deliverySequence: 1,
    occurredAtMs: 1_000,
    event,
  };
}

/** 构造独立 MCP OAuth JSON-RPC 通知；通知不绑定 Session 或 Turn。 */
function oauthNotification(
  event: Record<string, unknown>,
): Record<string, unknown> {
  return {
    jsonrpc: "2.0",
    method: "keencode/mcp/oauth",
    params: event,
  };
}

describe("ACP delivery parser", () => {
  it("按标准顶层 Content、Diff、Terminal 验证工具内容，不接受私有替代字段", () => {
    const content = [
      { type: "content" as const, content: { type: "text" as const, text: "完成" } },
      { type: "diff" as const, path: "file.txt", oldText: null, newText: "\uFEFF正文\r\n" },
      { type: "terminal" as const, terminalId: "terminal-1" },
    ];
    for (const update of [
      { sessionUpdate: "tool_call" as const, toolCallId: "call-1", title: "Write", content },
      { sessionUpdate: "tool_call_update" as const, toolCallId: "call-1", content },
    ]) {
      const envelope = updateEnvelope(update);
      expect(parseSessionUpdateDeliveryEnvelope(JSON.parse(JSON.stringify(envelope))))
        .toEqual(envelope);
    }
    for (const invalid of [
      { type: "content", content: { type: "diff", path: "x", newText: "new" } },
      { type: "diff", path: "x", patch: "@@", newText: "new" },
      { type: "diff", path: "x", oldText: 1, newText: "new" },
      { type: "diff", path: "x" },
      { type: "diff", path: "x", newText: "new", unexpected: true },
      { type: "terminal", terminalId: "" },
      { type: "content" },
    ]) {
      const envelope = updateEnvelope({
        sessionUpdate: "tool_call_update", toolCallId: "call-1",
      });
      expect(parseSessionUpdateDeliveryEnvelope({
        ...envelope, update: { ...envelope.update, content: [invalid] },
      })).toBeNull();
    }
  });

  it("严格校验文件变更 resource_link 的 URI 与快照引用，普通 resource_link 仍可透传", () => {
    const emptySha256 =
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const reference = {
      sessionId: "session-1",
      requestId: "request-1",
      path: "C:/workspace/empty.txt",
      before: null,
      after: { sizeBytes: 0, sha256: emptySha256 },
      applied: true,
    };
    const resourceLink = {
      type: "content" as const,
      content: {
        type: "resource_link" as const,
        name: "empty.txt",
        uri: fileChangeUri(reference.sessionId, reference.requestId),
        description: "已应用的持久文件快照",
        mimeType: "application/octet-stream",
        size: 0,
        title: "空文件",
        annotations: { audience: ["user"], priority: 0.5 },
        _meta: { "keencode/fileChange": reference },
      },
    };
    const valid = updateEnvelope({
      sessionUpdate: "tool_call_update",
      toolCallId: "call-1",
      content: [resourceLink],
    });
    expect(parseSessionUpdateDeliveryEnvelope(valid)).toEqual(valid);
    expect(parseSessionUpdateDeliveryEnvelope(updateEnvelope({
      sessionUpdate: "tool_call_update",
      toolCallId: "call-2",
      content: [{
        type: "content",
        content: {
          type: "resource_link",
          name: "docs",
          uri: "https://example.com/docs",
        },
      }],
    }))).not.toBeNull();

    const invalidLinks = [
      {
        ...resourceLink,
        content: {
          ...resourceLink.content,
          uri: "keencode://sessions/session-1/file-changes/other-request",
        },
      },
      {
        ...resourceLink,
        content: {
          ...resourceLink.content,
          _meta: {
            "keencode/fileChange": { ...reference, extra: true },
          },
        },
      },
      {
        ...resourceLink,
        content: {
          ...resourceLink.content,
          _meta: {
            "keencode/fileChange": {
              ...reference,
              after: { sizeBytes: 0, sha256: "0".repeat(64) },
            },
          },
        },
      },
    ];
    for (const content of invalidLinks) {
      expect(parseSessionUpdateDeliveryEnvelope(updateEnvelope({
        sessionUpdate: "tool_call_update",
        toolCallId: "call-invalid",
        content: [content],
      }))).toBeNull();
    }
  });

  it("接受严格的根 Turn 更新与不绑定 Turn 的 Session 更新", () => {
    const root = updateEnvelope({
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "完成" },
    });
    expect(parseSessionUpdateDeliveryEnvelope(root)).toEqual(root);

    const scoped = {
      schemaVersion: 1,
      sessionId: "session-1",
      deliverySequence: 2,
      occurredAtMs: 1_001,
      update: {
        sessionUpdate: "session_info_update",
        title: "新标题",
      },
    };
    expect(parseSessionUpdateDeliveryEnvelope(scoped)).toEqual(scoped);

    const plan = {
      schemaVersion: 1,
      sessionId: "session-1",
      deliverySequence: 3,
      occurredAtMs: 1_002,
      update: {
        sessionUpdate: "plan",
        entries: [{ content: "执行", priority: "high", status: "pending" }],
      },
    };
    expect(parseSessionUpdateDeliveryEnvelope(plan)).toEqual(plan);
    expect(parseSessionUpdateDeliveryEnvelope({
      ...plan,
      turnId: "turn-1",
      sourceAgentId: "root",
    })).toBeNull();
  });

  it("允许用户消息在 Session 级历史与 Turn 级回放之间保持两种作用域", () => {
    const user = {
      schemaVersion: 1,
      sessionId: "session-1",
      deliverySequence: 4,
      occurredAtMs: 1_003,
      update: {
        sessionUpdate: "user_message_chunk",
        content: { type: "text", text: "历史用户消息" },
      },
    };
    expect(parseSessionUpdateDeliveryEnvelope(user)).toEqual(user);
    expect(parseSessionUpdateDeliveryEnvelope({
      ...user,
      turnId: "turn-1",
      sourceAgentId: "root",
    })).toEqual({
      ...user,
      turnId: "turn-1",
      sourceAgentId: "root",
    });
  });

  it("拒绝缺失身份配对、首层多余字段与非法枚举", () => {
    const missingSource = updateEnvelope({
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "x" },
    });
    delete missingSource.sourceAgentId;
    expect(parseSessionUpdateDeliveryEnvelope(missingSource)).toBeNull();

    const extra = updateEnvelope({
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "x" },
    });
    extra.legacy = true;
    expect(parseSessionUpdateDeliveryEnvelope(extra)).toBeNull();

    expect(parseSessionUpdateDeliveryEnvelope(updateEnvelope({
      sessionUpdate: "tool_call",
      toolCallId: "tool-1",
      title: "Write",
      kind: "write" as never,
    }))).toBeNull();
    expect(parseSessionUpdateDeliveryEnvelope(updateEnvelope({
      sessionUpdate: "plan",
      entries: [{
        content: "执行",
        priority: "urgent" as never,
        status: "pending",
      }],
    }))).toBeNull();
  });

  it("拒绝内容块和更新内部的未知字段", () => {
    expect(parseSessionUpdateDeliveryEnvelope(updateEnvelope({
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "x", legacy: true } as never,
    }))).toBeNull();
    expect(parseSessionUpdateDeliveryEnvelope(updateEnvelope({
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "x" },
      messageId: "legacy-message-id",
    } as never))).toBeNull();
    expect(parseSessionUpdateDeliveryEnvelope(updateEnvelope({
      sessionUpdate: "usage_update",
      used: 10,
      size: 100,
      legacy: true,
    } as never))).toBeNull();
  });

  it("UsageUpdate 的 used 和 size 只接受安全整数", () => {
    const valid = updateEnvelope({
      sessionUpdate: "usage_update",
      used: Number.MAX_SAFE_INTEGER,
      size: Number.MAX_SAFE_INTEGER,
    });
    expect(parseSessionUpdateDeliveryEnvelope(valid)).toEqual(valid);

    for (const field of ["used", "size"] as const) {
      expect(parseSessionUpdateDeliveryEnvelope({
        ...valid,
        update: { ...valid.update, [field]: 1.5 },
      })).toBeNull();
      expect(parseSessionUpdateDeliveryEnvelope({
        ...valid,
        update: {
          ...valid.update,
          [field]: Number.MAX_SAFE_INTEGER + 1,
        },
      })).toBeNull();
    }

    expect(parseSessionUpdateDeliveryEnvelope({
      ...valid,
      update: { ...valid.update, used: 0 },
    })).toEqual({
      ...valid,
      update: { ...valid.update, used: 0 },
    });
  });

  it("权威事件强制 Journal 序号，Session 事件禁止伪造 Turn 身份", () => {
    const started = eventEnvelope({
      type: "turn_started",
      rootTurnId: "turn-1",
    });
    expect(parseKeenCodeEventEnvelope(started)).toEqual(started);
    delete started.journalSequence;
    expect(parseKeenCodeEventEnvelope(started)).toBeNull();

    const goal = {
      schemaVersion: 1,
      sessionId: "session-1",
      deliverySequence: 2,
      occurredAtMs: 1_001,
      event: {
        type: "goal_changed",
        goalId: "goal-1",
        revision: 2,
        status: "active",
      },
    };
    expect(parseKeenCodeEventEnvelope(goal)).toEqual(goal);
    expect(parseKeenCodeEventEnvelope({
      ...goal,
      turnId: "turn-1",
      sourceAgentId: "root",
    })).toBeNull();
  });

  it("拒绝生命周期非法状态和事件内部未知字段", () => {
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "agent_status_changed",
      agentId: "child-1",
      status: "paused" as never,
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "turn_failed",
      failureKind: "network" as never,
      message: "失败",
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "turn_completed",
      legacy: true,
    } as never))).toBeNull();
  });

  it("镜像 Rust 的 Turn、Agent 和压缩事件身份不变量", () => {
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "turn_started",
      rootTurnId: "other-turn",
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope({
      ...eventEnvelope({
        type: "turn_started",
        rootTurnId: "root-turn",
        parentTurnId: "root-turn",
      }),
      turnId: "child-turn",
      sourceAgentId: "child-agent",
    })).toEqual({
      ...eventEnvelope({
        type: "turn_started",
        rootTurnId: "root-turn",
        parentTurnId: "root-turn",
      }),
      turnId: "child-turn",
      sourceAgentId: "child-agent",
    });
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "agent_spawned",
      agentId: "child-agent",
      parentAgentId: "other-agent",
      agentPath: "root/child",
      task: "检查子任务",
      parentTurnId: "turn-1",
      rootTurnId: "turn-1",
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "agent_spawned",
      agentId: "child-agent",
      parentAgentId: "root",
      agentPath: "root/child",
      task: "检查子任务",
      parentTurnId: "turn-1",
      rootTurnId: "turn-1",
    }))).toEqual(eventEnvelope({
      type: "agent_spawned",
      agentId: "child-agent",
      parentAgentId: "root",
      agentPath: "root/child",
      task: "检查子任务",
      parentTurnId: "turn-1",
      rootTurnId: "turn-1",
    }));
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "agent_spawned",
      agentId: "child-agent",
      parentAgentId: "root",
      agentPath: "root/child",
      task: "检查子任务",
      parentTurnId: "turn-1",
      rootTurnId: "other-turn",
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "agent_message_queued",
      messageId: "message-1",
      fromAgentId: "root",
      toAgentId: "root",
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "context_compaction_completed",
      replacedThroughSequence: 1,
      estimatedTokens: 10,
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope({
      ...eventEnvelope({
        type: "context_compaction_completed",
        replacedThroughSequence: 1,
        estimatedTokens: 10,
      }),
      journalSequence: 2,
    })).not.toBeNull();
  });

  it("拒绝事件字段中的越界文本、控制字符和显式空值", () => {
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "turn_failed",
      failureKind: "model",
      message: "x".repeat(4097),
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "turn_failed",
      failureKind: "model",
      message: "失败\u0000",
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope({
      ...sessionEventEnvelope({
        type: "goal_changed",
        goalId: "goal-1",
        revision: 2,
        status: "active",
      }),
      journalSequence: null,
    })).toBeNull();
    expect(parseKeenCodeEventEnvelope(sessionEventEnvelope({
      type: "goal_changed",
      goalId: "goal-1",
      revision: 2,
    }))).toBeNull();
    expect(parseSessionUpdateDeliveryEnvelope({
      ...updateEnvelope({
        sessionUpdate: "agent_message_chunk",
        content: { type: "text", text: "x" },
      }),
      turnId: null,
      sourceAgentId: null,
    })).toBeNull();
  });

  it("接受最大 Agent 任务正文并拒绝超过 UTF-8 字节边界的正文", () => {
    // 与协作工具和 Rust ACP 事件校验共用的初始任务正文上限。
    const maxTaskBytes = 256 * 1024;
    const boundaryTask = "a".repeat(maxTaskBytes);
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "agent_spawned",
      agentId: "child-agent",
      parentAgentId: "root",
      agentPath: "root/child",
      task: boundaryTask,
      parentTurnId: "turn-1",
      rootTurnId: "turn-1",
    }))).not.toBeNull();
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "agent_spawned",
      agentId: "child-agent",
      parentAgentId: "root",
      agentPath: "root/child",
      task: boundaryTask + "a",
      parentTurnId: "turn-1",
      rootTurnId: "turn-1",
    }))).toBeNull();
  });

  it("严格校验独立 OAuth 通知、重试和后台任务的危险或越界字段", () => {
    for (const authorizationUrl of [
      "http://auth.example/authorize",
      "https://user:secret@auth.example/authorize",
      "https://auth.example/authorize?client_secret=secret",
      "https://auth.example/authorize#access_token=secret",
      "https://auth.example/authorize#",
      "https://auth.example/authorize bad",
    ]) {
      expect(parseAcpTauriDelivery({
        type: "notification",
        notification: oauthNotification({
        type: "mcp_oauth_authorization_required",
        projectPath: "C:/projects/demo",
        serverName: "docs",
        authorizationUrl,
        }),
      })).toBeNull();
    }
    expect(parseAcpTauriDelivery({
      type: "notification",
      notification: oauthNotification({
        type: "mcp_oauth_authorization_required",
        projectPath: "C:/projects/demo",
        serverName: "local",
        authorizationUrl: "http://127.0.0.1:3000/authorize",
      }),
    })).not.toBeNull();
    expect(parseAcpTauriDelivery({
      type: "notification",
      notification: oauthNotification({
        type: "mcp_oauth_authorized",
        projectPath: "C:/projects/demo",
        serverName: "local",
      }),
    })).not.toBeNull();
    expect(parseAcpTauriDelivery({
      type: "notification",
      notification: oauthNotification({
        type: "mcp_oauth_failed",
        projectPath: "C:/projects/demo",
        serverName: "local",
        message: "授权被拒绝",
      }),
    })).not.toBeNull();
    const maxProjectPath = `C:/${"a".repeat(4 * 1024 - 3)}`;
    expect(parseAcpTauriDelivery({
      type: "notification",
      notification: oauthNotification({
        type: "mcp_oauth_authorized",
        projectPath: maxProjectPath,
        serverName: "local",
      }),
    })).not.toBeNull();
    expect(parseAcpTauriDelivery({
      type: "notification",
      notification: oauthNotification({
        type: "mcp_oauth_authorized",
        projectPath: `${maxProjectPath}a`,
        serverName: "local",
      }),
    })).toBeNull();
    expect(parseAcpTauriDelivery({
      type: "notification",
      notification: oauthNotification({
        type: "mcp_oauth_authorized",
        serverName: "local",
      }),
    })).toBeNull();
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "model_retry_scheduled",
      attempt: 3,
      maxAttempts: 3,
      delayMs: 100,
      message: "稍后重试",
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope(eventEnvelope({
      type: "model_retry_scheduled",
      attempt: 1,
      maxAttempts: 33,
      delayMs: 100,
      message: "稍后重试",
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope(sessionEventEnvelope({
      type: "background_task_completed",
      taskId: "task-1",
      taskKind: "shell",
      agentId: "agent-1",
      status: "failed",
      durationMs: 100,
      summary: "命令失败",
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope(sessionEventEnvelope({
      type: "background_task_completed",
      taskId: "task-1",
      taskKind: "shell",
      status: "failed",
      durationMs: 100,
      summary: "Authorization: Bearer unredacted-token-value",
    }))).toBeNull();
    expect(parseKeenCodeEventEnvelope(sessionEventEnvelope({
      type: "background_task_completed",
      taskId: "task-1",
      taskKind: "agent",
      status: "succeeded",
      durationMs: 100,
      summary: "完成",
    }))).toBeNull();
  });

  it("唯一 Tauri 联合拒绝旧 OAuth Session 信封、旧事件名和二次 JSON 字符串", () => {
    const envelope = updateEnvelope({
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "x" },
    });
    expect(parseAcpTauriDelivery({ type: "session_update", envelope })).toEqual({
      type: "session_update",
      envelope,
    });
    expect(parseAcpTauriDelivery({ type: "unknown", envelope })).toBeNull();
    expect(parseAcpTauriDelivery(JSON.stringify({
      type: "session_update",
      envelope,
    }))).toBeNull();
    expect(parseAcpTauriDelivery({
      type: "keencode_event",
      envelope: sessionEventEnvelope({
        type: "mcp_oauth_authorized",
        serverName: "legacy",
      } as never),
    })).toBeNull();
    expect(parseAcpTauriDelivery({
      type: "notification",
      notification: oauthNotification({
        type: "mcp_oauth_authorized",
        projectPath: "C:/projects/demo",
        serverName: "docs",
      }),
      legacy: true,
    })).toBeNull();
  });
});

describe("ACP Client request parser", () => {
  it("接受字符串或整数 JSON-RPC ID 的表单问答", () => {
    const request = {
      jsonrpc: "2.0",
      id: 7,
      method: "elicitation/create",
      params: {
        mode: "form",
        sessionId: "session-1",
        message: "请选择",
        requestedSchema: {
          type: "object",
          properties: { target: { type: "string" } },
          required: ["target"],
        },
      },
    };
    expect(parseAcpJsonRpcClientRequest(request)).toEqual(request);
    expect(parseAcpTauriDelivery({ type: "client_request", request })).toEqual({
      type: "client_request",
      request,
    });
    expect(parseAcpJsonRpcClientRequest({ ...request, id: 1.5 })).toBeNull();
    expect(parseAcpJsonRpcClientRequest({ ...request, rpcId: 7 })).toBeNull();
  });
});

describe("ACP event semantics", () => {
  it("区分 Session 级、权威和终态事件", () => {
    expect(isSessionScopedUpdate({
      sessionUpdate: "session_info_update",
      title: "标题",
    })).toBe(true);
    expect(isSessionScopedUpdate({
      sessionUpdate: "plan",
      entries: [],
    })).toBe(true);
    expect(isSessionScopedUpdate({
      sessionUpdate: "usage_update",
      used: 1,
      size: 10,
    })).toBe(false);
    expect(isSessionScopedKeenCodeEvent({
      type: "system_notification",
      level: "warning",
      message: "恢复提示",
    })).toBe(true);
    expect(isAuthoritativeKeenCodeEvent({ type: "turn_completed" })).toBe(true);
    expect(isTerminalKeenCodeEvent({ type: "turn_cancelled" })).toBe(true);
    expect(isTerminalKeenCodeEvent({
      type: "model_first_stream_observed",
    })).toBe(false);
  });

  it("子 Agent 更新不驱动根 Session streaming", () => {
    const update: SessionUpdate = {
      sessionUpdate: "tool_call",
      toolCallId: "tool-1",
      title: "Read",
    };
    expect(shouldDriveMainSessionStreaming(update, false)).toBe(true);
    expect(shouldDriveMainSessionStreaming(update, true)).toBe(false);
    expect(shouldDriveMainSessionStreaming({
      sessionUpdate: "usage_update",
      used: 1,
      size: 10,
    }, false)).toBe(false);
    expect(shouldDriveMainSessionStreaming({
      sessionUpdate: "plan",
      entries: [],
    }, false)).toBe(false);
  });

  it("系统通知允许 Session 级或 Turn 级身份，但拒绝部分身份", () => {
    const event: KeenCodeEvent = {
      type: "system_notification",
      level: "warning",
      message: "恢复提示",
    };
    expect(parseKeenCodeEventEnvelope(sessionEventEnvelope(event))).not.toBeNull();
    const turn = {
      ...sessionEventEnvelope(event),
      turnId: "turn-1",
      sourceAgentId: "root",
    };
    expect(parseKeenCodeEventEnvelope(turn)).not.toBeNull();
    const partial = { ...sessionEventEnvelope(event), turnId: "turn-1" };
    expect(parseKeenCodeEventEnvelope(partial)).toBeNull();
  });

  it("仅合并同类型的连续文本更新", () => {
    const first: SessionUpdate = {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "你" },
    };
    const second: SessionUpdate = {
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "好" },
    };
    expect(mergeSessionTextUpdates(first, second)).toEqual({
      sessionUpdate: "agent_message_chunk",
      content: { type: "text", text: "你好" },
    });
    expect(mergeSessionTextUpdates(first, {
      sessionUpdate: "agent_thought_chunk",
      content: { type: "text", text: "思考" },
    })).toBeNull();
  });
});

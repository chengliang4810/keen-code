import { beforeEach, describe, expect, it, vi } from "vitest";

/** client.ts 的 ACP 请求桩；测试 API 层的参数映射而非重复测试传输实现。 */
const clientMocks = vi.hoisted(() => ({
  /** 初始化握手桩。 */
  acpInitialize: vi.fn(),
  /** 标准/扩展请求桩。 */
  acpRequest: vi.fn(),
  /** 标准通知桩。 */
  acpNotify: vi.fn(),
  /** Client 响应桩；API 层只负责原样委派。 */
  acpRespond: vi.fn(),
}));

/** prompt.ts 的 Prompt 句柄桩；started/completed 生命周期由专属测试覆盖。 */
const promptMocks = vi.hoisted(() => ({
  /** 标准 Session Prompt 启动桩。 */
  startSessionPrompt: vi.fn(),
}));

vi.mock("./client", () => clientMocks);
vi.mock("./prompt", () => promptMocks);

import {
  acceptedElicitationResponse,
  acpClientRespond,
  cancelledClientResponse,
  createOperationId,
  goalClear,
  goalGet,
  goalTransition,
  goalUpsert,
  sessionConnect,
  sessionLoad,
  sessionDelete,
  sessionFork,
  sessionGenerateTitle,
  sessionMcpLoad,
  sessionMcpStatus,
  sessionMcpUnload,
  sessionRename,
  sessionRewind,
  sessionSend,
  sessionSetEffort,
  sessionSetModel,
  sessionStop,
  sessionSteer,
  sessionsList,
} from "./api";

/** 构造标准 ACP Host 返回的最小、严格命名空间 Session 快照。 */
function sessionSnapshot(
  sessionId = "session-1",
  overrides: Record<string, unknown> = {},
) {
  return {
    sessionId,
    state: "ready" as const,
    activeTurnId: null,
    backend: "acp" as const,
    projectPath: "D:/workspace",
    title: null,
    lastError: null,
    ...overrides,
  };
}

/** 构造带 Session 快照元数据的标准 new/load 响应。 */
function sessionResult(sessionId = "session-1", snapshotId = sessionId) {
  return {
    sessionId,
    _meta: { "keencode/snapshot": sessionSnapshot(snapshotId) },
  };
}

/** 为每个用例恢复默认 ACP 请求响应，避免用例间共享 mock 行为。 */
beforeEach(async () => {
  vi.resetAllMocks();
  clientMocks.acpInitialize.mockResolvedValue({ protocolVersion: 1 });
  clientMocks.acpRequest.mockResolvedValue({ sessions: [] });
  await sessionsList();
  clientMocks.acpRequest.mockReset().mockResolvedValue({});
  clientMocks.acpNotify.mockResolvedValue(undefined);
  clientMocks.acpRespond.mockResolvedValue(undefined);
  promptMocks.startSessionPrompt.mockReturnValue({
    started: Promise.resolve({ turnId: "turn-1", occurredAtMs: 1 }),
    completed: Promise.resolve({ stopReason: "end_turn" }),
  });
});

describe("ACP Session 标准 API 映射", () => {
  it("标题候选通过唯一 ACP 扩展传递，并将业务操作标识放入元数据", async () => {
    clientMocks.acpRequest.mockResolvedValue({ title: "修复重试逻辑" });
    await expect(sessionGenerateTitle({
      id: "session-1", userMessage: "补齐失败重试", operationId: "title-operation",
    })).resolves.toBe("修复重试逻辑");
    expect(clientMocks.acpRequest).toHaveBeenCalledWith("keencode/session/title", {
      sessionId: "session-1", userMessage: "补齐失败重试",
      _meta: { "keencode/operationId": "title-operation" },
    });
  });

  it("拒绝没有有效标题的 ACP 成功信封", async () => {
    clientMocks.acpRequest.mockResolvedValue({ title: " " });
    await expect(sessionGenerateTitle({
      id: "session-1", userMessage: "补齐失败重试", operationId: "title-operation",
    })).rejects.toThrow("ACP 标题响应缺少有效标题");
  });

  it("使用明确 projectPath 作为 session/new cwd，并返回严格快照", async () => {
    const result = sessionResult("session-new");
    clientMocks.acpInitialize.mockResolvedValue({
      protocolVersion: 1,
      _meta: { "keencode/defaultCwd": "D:/default" },
    });
    clientMocks.acpRequest.mockResolvedValue(result);

    await expect(
      sessionConnect({
        projectPath: "D:/explicit-project",
        operationId: "connect-1",
      }),
    ).resolves.toEqual(sessionSnapshot("session-new"));
    expect(clientMocks.acpRequest).toHaveBeenCalledWith(
      "session/new",
      {
        cwd: "D:/explicit-project",
        mcpServers: [],
        _meta: { "keencode/operationId": "connect-1" },
      },
      "connect-1",
    );
  });

  it("没有项目路径时只使用 Host 提供的 defaultCwd", async () => {
    const result = sessionResult("session-default");
    clientMocks.acpInitialize.mockResolvedValue({
      protocolVersion: 1,
      _meta: { "keencode/defaultCwd": "D:/host-default" },
    });
    clientMocks.acpRequest.mockResolvedValue(result);

    await sessionConnect({ operationId: "connect-default" });

    expect(clientMocks.acpRequest).toHaveBeenCalledWith(
      "session/new",
      expect.objectContaining({ cwd: "D:/host-default" }),
      "connect-default",
    );
  });

  it("加载既有 Session 时从 session/list 读取权威 cwd，不猜测项目路径", async () => {
    const loaded = {
      _meta: { "keencode/snapshot": sessionSnapshot("session-existing") },
    };
    clientMocks.acpRequest
      .mockResolvedValueOnce({
        sessions: [{
          sessionId: "session-existing",
          cwd: "D:/authoritative-project",
          title: "历史会话",
          updatedAt: "2026-09-05T00:00:00Z",
        }],
      })
      .mockResolvedValueOnce(loaded);

    await expect(
      sessionConnect({
        sessionId: "session-existing",
        projectPath: "D:/caller-path-must-not-be-used",
        operationId: "load-1",
      }),
    ).resolves.toEqual(sessionSnapshot("session-existing"));
    expect(clientMocks.acpInitialize).not.toHaveBeenCalled();
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      1,
      "session/list",
      {},
    );
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      2,
      "session/load",
      {
        sessionId: "session-existing",
        cwd: "D:/authoritative-project",
        mcpServers: [],
      },
    );
  });

  it("标准 new、load 和 fork 原样传递 Session MCP 完整配置", async () => {
    const mcpServers = [{
      name: "local",
      command: "mcp-server",
      args: ["--stdio"],
      env: [{ name: "MODE", value: "test" }],
    }];
    clientMocks.acpInitialize.mockResolvedValue({
      protocolVersion: 1,
      _meta: { "keencode/defaultCwd": "D:/default" },
    });
    clientMocks.acpRequest.mockResolvedValueOnce(sessionResult("session-new"));
    await sessionConnect({ operationId: "new-mcp", mcpServers });
    expect(clientMocks.acpRequest).toHaveBeenLastCalledWith(
      "session/new",
      {
        cwd: "D:/default",
        mcpServers,
        _meta: { "keencode/operationId": "new-mcp" },
      },
      "new-mcp",
    );

    clientMocks.acpRequest
      .mockResolvedValueOnce({ sessions: [{ sessionId: "existing", cwd: "D:/existing" }] });
    await sessionsList();
    clientMocks.acpRequest.mockClear().mockResolvedValueOnce({
      sessionId: "existing",
      _meta: {
        "keencode/snapshot": sessionSnapshot("existing", {
          projectPath: "D:/existing",
        }),
      },
    });
    await sessionConnect({ sessionId: "existing", operationId: "load-mcp", mcpServers });
    expect(clientMocks.acpRequest).toHaveBeenCalledExactlyOnceWith("session/load", {
      sessionId: "existing",
      cwd: "D:/existing",
      mcpServers,
    });

    clientMocks.acpRequest.mockReset().mockResolvedValueOnce({ sessionId: "forked" });
    await sessionFork({ sourceId: "existing", operationId: "fork-mcp", mcpServers });
    expect(clientMocks.acpRequest).toHaveBeenCalledExactlyOnceWith(
      "session/fork",
      {
        sessionId: "existing",
        cwd: "D:/existing",
        mcpServers,
        _meta: { "keencode/operationId": "fork-mcp" },
      },
      "fork-mcp",
    );
  });

  it("打开已列出的对话直接 load，不重复扫描列表", async () => {
    clientMocks.acpRequest.mockResolvedValueOnce({ sessions: [{ sessionId: "cached", cwd: "D:/cached" }] });
    await sessionsList();
    clientMocks.acpRequest.mockClear().mockResolvedValueOnce(sessionResult("cached"));
    await sessionConnect({ sessionId: "cached", operationId: "load-cached" });
    expect(clientMocks.acpRequest).toHaveBeenCalledExactlyOnceWith("session/load", {
      sessionId: "cached", cwd: "D:/cached", mcpServers: [],
    });
  });

  it("拒绝 new 响应中不一致的 Session ID 和旧快照字段别名", async () => {
    clientMocks.acpRequest.mockResolvedValueOnce({
      sessionId: "session-new",
      _meta: {
        "keencode/snapshot": sessionSnapshot("session-other"),
      },
    });
    await expect(
      sessionConnect({ projectPath: "D:/workspace", operationId: "new-id" }),
    ).rejects.toThrow("ACP 新会话标识不一致");

    clientMocks.acpRequest.mockResolvedValueOnce({
      sessionId: "session-new",
      _meta: {
        "keencode/snapshot": {
          sessionId: "session-new",
          state: "ready",
          activeTurnId: null,
          backend: "acp",
          cwd: "D:/workspace",
          status: "ready",
          title: null,
          lastError: null,
        },
      },
    });
    await expect(
      sessionConnect({ projectPath: "D:/workspace", operationId: "old-shape" }),
    ).rejects.toThrow("ACP Session 快照字段无效");
  });

  it("按 nextCursor 循环读取完整 session/list 页面", async () => {
    clientMocks.acpRequest
      .mockResolvedValueOnce({
        sessions: [{ sessionId: "session-1", cwd: "D:/one" }],
        nextCursor: "cursor-1",
      })
      .mockResolvedValueOnce({
        sessions: [{
          sessionId: "session-2",
          cwd: "D:/two",
          title: "第二个",
          updatedAt: "2026-09-05T00:00:00Z",
        }],
      });

    await expect(sessionsList()).resolves.toEqual([
      { id: "session-1", cwd: "D:/one", title: null, updatedAt: "" },
      {
        id: "session-2",
        cwd: "D:/two",
        title: "第二个",
        updatedAt: "2026-09-05T00:00:00Z",
      },
    ]);
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      1,
      "session/list",
      {},
    );
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      2,
      "session/list",
      { cursor: "cursor-1" },
    );
  });

  it("拒绝不推进的 session/list 游标循环", async () => {
    clientMocks.acpRequest.mockResolvedValue({
      sessions: [],
      nextCursor: "same-cursor",
    });

    await expect(sessionsList()).rejects.toThrow("ACP Session 列表游标未推进");
    expect(clientMocks.acpRequest).toHaveBeenCalledTimes(2);
  });

  it("用标准 session/set_config_option 设置模型和推理强度", async () => {
    await sessionSetModel({
      sessionId: "session-1",
      providerId: "provider-a",
      modelId: "model-a",
      operationId: "model-op",
    });
    await sessionSetEffort({
      sessionId: "session-1",
      effort: "high",
      operationId: "effort-op",
    });

    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      1,
      "session/set_config_option",
      {
        sessionId: "session-1",
        configId: "model",
        value: "provider-a::model-a",
        _meta: { "keencode/operationId": "model-op" },
      },
      "model-op",
    );
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      2,
      "session/set_config_option",
      {
        sessionId: "session-1",
        configId: "reasoning_effort",
        value: "high",
        _meta: { "keencode/operationId": "effort-op" },
      },
      "effort-op",
    );
  });

  it("使用权威 cwd 和元数据调用标准 session/fork", async () => {
    clientMocks.acpRequest
      .mockResolvedValueOnce({
        sessions: [{ sessionId: "source", cwd: "D:/source" }],
      })
      .mockResolvedValueOnce({ sessionId: "forked" });

    await expect(
      sessionFork({
        sourceId: "source",
        title: "Fork title",
        operationId: "fork-op",
      }),
    ).resolves.toEqual({ id: "forked" });
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      2,
      "session/fork",
      {
        sessionId: "source",
        cwd: "D:/source",
        mcpServers: [],
        _meta: {
          "keencode/operationId": "fork-op",
          "keencode/title": "Fork title",
        },
      },
      "fork-op",
    );
  });

  it("使用标准 session/delete，并把幂等标识留在 JSON-RPC ID", async () => {
    await expect(
      sessionDelete({ id: "session-old", operationId: "delete-op" }),
    ).resolves.toBeUndefined();
    expect(clientMocks.acpRequest).toHaveBeenCalledWith(
      "session/delete",
      { sessionId: "session-old" },
      "delete-op",
    );
  });
});

describe("ACP KeenCode 扩展和 Prompt API 映射", () => {
  it("映射 Session MCP load、status 和 unload 的唯一扩展方法", async () => {
    const mcpServers = [{
      type: "http" as const,
      name: "remote",
      url: "https://example.test/mcp",
      headers: [{ name: "Authorization", value: "Bearer secret" }],
    }];
    clientMocks.acpRequest
      .mockResolvedValueOnce({
        sessionId: "session-1",
        catalogGeneration: 1,
        servers: [],
        changed: true,
        deduplicated: false,
      })
      .mockResolvedValueOnce({
        sessionId: "session-1",
        catalogGeneration: 2,
        servers: [],
      })
      .mockResolvedValueOnce({
        sessionId: "session-1",
        catalogGeneration: 2,
        servers: [],
        changed: true,
        deduplicated: false,
      });

    await sessionMcpLoad({ sessionId: "session-1", mcpServers, operationId: "load-mcp" });
    await sessionMcpStatus("session-1");
    await sessionMcpUnload({
      sessionId: "session-1",
      serverName: "remote",
      operationId: "unload-mcp",
    });

    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      1,
      "keencode/session/mcp/load",
      {
        sessionId: "session-1",
        mcpServers,
        _meta: { "keencode/operationId": "load-mcp" },
      },
      "load-mcp",
    );
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      2,
      "keencode/session/mcp/status",
      { sessionId: "session-1" },
    );
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      3,
      "keencode/session/mcp/unload",
      {
        sessionId: "session-1",
        serverName: "remote",
        _meta: { "keencode/operationId": "unload-mcp" },
      },
      "unload-mcp",
    );
  });

  it("通过 keencode/session/steer 发送引导文本，并把业务操作 ID 放入元数据", async () => {
    await sessionSteer({
      text: "继续检查",
      sessionId: "session-1",
      operationId: "steer-op",
    });
    expect(clientMocks.acpRequest).toHaveBeenCalledWith(
      "keencode/session/steer",
      {
        sessionId: "session-1",
        text: "继续检查",
        _meta: { "keencode/operationId": "steer-op" },
      },
    );
  });

  it.each([false, true])(
    "通过 keencode/session/rewind 按消息 id 回退并透传 revertFiles=%s",
    async (revertFiles) => {
    const rewind = {
      sessionId: "session-1",
      archivedSessionId: "archive-1",
      throughJournalSequence: 7,
      revertedFiles: revertFiles,
    };
    const args = {
      sessionId: "session-1",
      targetMessageId: "message-2",
      expectedText: "原始正文\n@D:/workspace/file.txt",
      revertFiles,
      operationId: "rewind-op",
    };
    clientMocks.acpRequest.mockResolvedValue(rewind);

    await expect(sessionRewind(args)).resolves.toEqual(rewind);
    expect(clientMocks.acpRequest).toHaveBeenCalledWith(
      "keencode/session/rewind",
      {
        sessionId: "session-1",
        targetMessageId: "message-2",
        expectedText: "原始正文\n@D:/workspace/file.txt",
        revertFiles,
        _meta: { "keencode/operationId": "rewind-op" },
      },
    );
    },
  );

  it("拒绝缺少 archivedSessionId 的 rewind 响应", async () => {
    clientMocks.acpRequest.mockResolvedValue({
      sessionId: "session-1",
      throughJournalSequence: 7,
      revertedFiles: false,
    });

    await expect(
      sessionRewind({
        sessionId: "session-1",
        targetMessageId: "message-2",
        expectedText: "原始正文",
        revertFiles: false,
        operationId: "rewind-missing-archive",
      }),
    ).rejects.toThrow("响应格式无效");
  });

  it("拒绝与请求 Session 不一致的 rewind 响应", async () => {
    clientMocks.acpRequest.mockResolvedValue({
      sessionId: "session-other",
      archivedSessionId: "archive-1",
      throughJournalSequence: 7,
      revertedFiles: false,
    });

    await expect(
      sessionRewind({
        sessionId: "session-1",
        targetMessageId: "message-2",
        expectedText: "原始正文",
        revertFiles: false,
        operationId: "rewind-wrong-session",
      }),
    ).rejects.toThrow("响应格式无效");
  });

  it("拒绝包含未知字段的 rewind 响应", async () => {
    clientMocks.acpRequest.mockResolvedValue({
      sessionId: "session-1",
      archivedSessionId: "archive-1",
      throughJournalSequence: 7,
      revertedFiles: false,
      extra: true,
    });

    await expect(
      sessionRewind({
        sessionId: "session-1",
        targetMessageId: "message-2",
        expectedText: "原始正文",
        revertFiles: false,
        operationId: "rewind-unknown-field",
      }),
    ).rejects.toThrow("响应格式无效");
  });

  it("拒绝归档 Session 与源 Session 相同的 rewind 响应", async () => {
    clientMocks.acpRequest.mockResolvedValue({
      sessionId: "session-1",
      archivedSessionId: "session-1",
      throughJournalSequence: 7,
      revertedFiles: false,
    });

    await expect(
      sessionRewind({
        sessionId: "session-1",
        targetMessageId: "message-2",
        expectedText: "原始正文",
        revertFiles: false,
        operationId: "rewind-same-session",
      }),
    ).rejects.toThrow("响应格式无效");
  });

  it.each([
    ["sessionId", "session-\u0001"],
    ["archivedSessionId", "archive-\u0001"],
  ])("拒绝 %s 含控制字符的 rewind 响应", (field, invalidValue) => {
    const response = {
      sessionId: "session-1",
      archivedSessionId: "archive-1",
      throughJournalSequence: 7,
      revertedFiles: false,
      [field]: invalidValue,
    };
    clientMocks.acpRequest.mockResolvedValue(response);

    return expect(
      sessionRewind({
        sessionId: "session-1",
        targetMessageId: "message-2",
        expectedText: "原始正文",
        revertFiles: false,
        operationId: `rewind-invalid-${field}-control`,
      }),
    ).rejects.toThrow("响应格式无效");
  });

  it.each(["sessionId", "archivedSessionId"])(
    "拒绝 %s 超过 256 UTF-8 字节的 rewind 响应",
    (field) => {
      const response = {
        sessionId: "session-1",
        archivedSessionId: "archive-1",
        throughJournalSequence: 7,
        revertedFiles: false,
        [field]: "x".repeat(257),
      };
      clientMocks.acpRequest.mockResolvedValue(response);

      return expect(
        sessionRewind({
          sessionId: "session-1",
          targetMessageId: "message-2",
          expectedText: "原始正文",
          revertFiles: false,
          operationId: `rewind-invalid-${field}-length`,
        }),
      ).rejects.toThrow("响应格式无效");
    },
  );

  it.each(["", "   "])(
    "拒绝 archivedSessionId=%j 的非法 rewind 响应",
    async (archivedSessionId) => {
      clientMocks.acpRequest.mockResolvedValue({
        sessionId: "session-1",
        archivedSessionId,
        throughJournalSequence: 7,
        revertedFiles: false,
      });

      await expect(
        sessionRewind({
          sessionId: "session-1",
          targetMessageId: "message-2",
          expectedText: "原始正文",
          revertFiles: false,
          operationId: "rewind-invalid-archive",
        }),
      ).rejects.toThrow("响应格式无效");
    },
  );

  it.each([undefined, 0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, Number.NaN, Number.POSITIVE_INFINITY, "7"])(
    "拒绝 throughJournalSequence=%s 的非法 rewind 响应",
    async (throughJournalSequence) => {
      clientMocks.acpRequest.mockResolvedValue({
        sessionId: "session-1",
        archivedSessionId: "archive-1",
        ...(throughJournalSequence === undefined ? {} : { throughJournalSequence }),
        revertedFiles: false,
      });

      await expect(
        sessionRewind({
          sessionId: "session-1",
          targetMessageId: "message-2",
          expectedText: "原始正文",
          revertFiles: false,
          operationId: "rewind-invalid-sequence",
        }),
      ).rejects.toThrow("响应格式无效");
    },
  );

  it.each([undefined, "false", 0])(
    "拒绝 revertedFiles=%s 的非法 rewind 响应",
    async (revertedFiles) => {
      clientMocks.acpRequest.mockResolvedValue({
        sessionId: "session-1",
        archivedSessionId: "archive-1",
        throughJournalSequence: 7,
        ...(revertedFiles === undefined ? {} : { revertedFiles }),
      });

      await expect(
        sessionRewind({
          sessionId: "session-1",
          targetMessageId: "message-2",
          expectedText: "原始正文",
          revertFiles: true,
          operationId: "rewind-invalid-reverted-files",
        }),
      ).rejects.toThrow("响应格式无效");
    },
  );

  it("acpClientRespond 直接委派完整 Client 响应，不改变协议载荷", async () => {
    const response = {
      jsonrpc: "2.0" as const,
      id: "elicitation-1",
      result: {
        action: "accept" as const,
        content: { target: "local", scopes: ["read", "write"] },
      },
    };

    await expect(acpClientRespond(response)).resolves.toBeUndefined();
    expect(clientMocks.acpRespond).toHaveBeenCalledOnce();
    expect(clientMocks.acpRespond).toHaveBeenCalledWith(response);
  });

  it("直接返回类型化 rename 结果，并把业务操作 ID 放入元数据", async () => {
    const renamed = {
      sessionId: "session-1",
      title: "新标题",
      journalSequence: 42,
    };
    clientMocks.acpRequest.mockResolvedValue(renamed);

    await expect(
      sessionRename({
        id: "session-1",
        title: "新标题",
        operationId: "rename-op",
      }),
    ).resolves.toEqual(renamed);
    expect(clientMocks.acpRequest).toHaveBeenCalledOnce();
    expect(clientMocks.acpRequest).toHaveBeenCalledWith(
      "keencode/session/rename",
      {
        sessionId: "session-1",
        title: "新标题",
        _meta: { "keencode/operationId": "rename-op" },
      },
    );
  });

  it("通过 keencode Goal 方法传递幂等 JSON-RPC ID", async () => {
    const goal = {
      sessionId: "session-1",
      revision: 3,
      goal: undefined,
      deduplicated: false,
    };
    const input = {
      sessionId: "session-1",
      goal: { title: "目标", objective: "完成目标" },
      expectedRevision: 2,
      requestNonce: "goal-upsert-op",
    };
    clientMocks.acpRequest.mockResolvedValue(goal);

    await expect(goalGet("session-1")).resolves.toEqual(goal);
    await expect(goalUpsert(input)).resolves.toEqual(goal);
    await expect(
      goalTransition({
        sessionId: "session-1",
        goalId: "goal-1",
        status: "blocked",
        reason: "缺少凭据",
        expectedRevision: 3,
        requestNonce: "goal-transition-op",
      }),
    ).resolves.toEqual(goal);
    await expect(
      goalClear({
        sessionId: "session-1",
        expectedRevision: 3,
        requestNonce: "goal-clear-op",
      }),
    ).resolves.toEqual(goal);

    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      1,
      "keencode/goal/get",
      { sessionId: "session-1" },
    );
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      2,
      "keencode/goal/upsert",
      input,
      "goal-upsert-op",
    );
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      3,
      "keencode/goal/transition",
      expect.objectContaining({ requestNonce: "goal-transition-op" }),
      "goal-transition-op",
    );
    expect(clientMocks.acpRequest).toHaveBeenNthCalledWith(
      4,
      "keencode/goal/clear",
      {
        sessionId: "session-1",
        expectedRevision: 3,
        requestNonce: "goal-clear-op",
      },
      "goal-clear-op",
    );
  });

  it("sessionStop 使用无响应的标准取消通知，不返回伪造快照", async () => {
    await expect(sessionStop("session-1", "turn-1")).resolves.toBeUndefined();
    expect(clientMocks.acpNotify).toHaveBeenCalledWith(
      "session/cancel",
      {
        sessionId: "session-1",
        _meta: { "keencode/turnId": "turn-1" },
      },
    );
    expect(clientMocks.acpRequest).not.toHaveBeenCalled();
  });

  it("sessionSend 原样委托 startSessionPrompt 并返回 started/completed 句柄", () => {
    const run = {
      started: Promise.resolve({ turnId: "turn-1", occurredAtMs: 123 }),
      completed: Promise.resolve({ stopReason: "end_turn" as const }),
    };
    const args = {
      text: "检查项目",
      sessionId: "session-1",
      requestId: "turn-1",
      planMode: true,
      ultraMode: false,
    };
    promptMocks.startSessionPrompt.mockReturnValue(run);

    expect(sessionSend(args)).toBe(run);
    expect(promptMocks.startSessionPrompt).toHaveBeenCalledWith(args);
  });
});

describe("ACP 辅助构造", () => {
  it("生成带作用域且不含空白的唯一 operationId", () => {
    const operationId = createOperationId("session-connect");
    expect(operationId).toMatch(/^session-connect-[0-9a-f-]{36}$/);
    expect(() => createOperationId("session connect")).toThrow(
      "operationId scope",
    );
  });

  it("只构造标准 Elicitation 接受与取消响应", () => {
    expect(acceptedElicitationResponse("ask-1", { target: "local" })).toEqual({
      jsonrpc: "2.0",
      id: "ask-1",
      result: { action: "accept", content: { target: "local" } },
    });
    expect(cancelledClientResponse("ask-2")).toEqual({
      jsonrpc: "2.0",
      id: "ask-2",
      result: { action: "cancel" },
    });
  });
});


it("session/load 分页参数进入命名空间元数据，无参数保留标准请求", async () => {
  clientMocks.acpRequest.mockResolvedValueOnce({ sessions: [{ sessionId: "paged-api", cwd: "D:/paged" }] });
  await sessionsList();
  clientMocks.acpRequest.mockClear().mockResolvedValue({});
  await sessionLoad("paged-api", { limit: 2 });
  expect(clientMocks.acpRequest).toHaveBeenLastCalledWith("session/load", {
    sessionId: "paged-api", cwd: "D:/paged", mcpServers: [], _meta: { "keencode/history": { limit: 2 } },
  });
  await sessionLoad("paged-api", { limit: -1, cursor: "next" });
  expect(clientMocks.acpRequest).toHaveBeenLastCalledWith("session/load", {
    sessionId: "paged-api", cwd: "D:/paged", mcpServers: [], _meta: { "keencode/history": { limit: -1, cursor: "next" } },
  });
  for (const limit of [0, -2, 101, 1.5]) await expect(sessionLoad("paged-api", { limit })).rejects.toThrow();
});

import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  customInstructionsGet,
  customInstructionsSet,
  memoriesGet,
  memoriesSet,
  requestRecordsList,
  fsWriteAbsolute,
  fsWriteFile,
  gitCommit,
  gitPush,
  gitStatus,
  gitUntrackedDirectory,
  agentDetail,
  agentsList,
  inspectMcp,
  mcpDoctor,
  pluginsList,
  settingsGet,
  settingsSet,
  taskCacheUsageGet,
  type BackgroundTaskInfo,
  type GitStatusResult,
} from "./api";

/** 构造测试使用的空 Git 状态。 */
function gitStatusResult(branch = "main"): GitStatusResult {
  return {
    available: true,
    files: [],
    branch,
    branches: [branch],
    additions: 0,
    deletions: 0,
    hasUnstagedChanges: false,
  };
}

/** 构造文件写入成功结果。 */
function fsWriteResult(absolutePath: string) {
  return {
    relativePath: "src/file.ts",
    absolutePath,
    size: 12,
    mtimeMs: 1_723_600_000_000,
  };
}

/** 构造后台任务列表 ACP 的完整 DTO，覆盖所有前端可消费字段。 */
function backgroundTaskResult(): BackgroundTaskInfo {
  return {
    sessionId: "session-background",
    taskId: "task-shell-1",
    kind: "shell",
    childThreadId: null,
    summary: "pnpm test",
    startedAt: "2026-09-05T00:00:00Z",
    durationMs: 1_250,
    pid: 1234,
  };
}

/** 读取真实 Tauri 入口，避免 handler 注册回归测试依赖构建产物。 */
function readTauriEntrySource(): string {
  return readFileSync(
    new URL("../../src-tauri/src/lib.rs", import.meta.url),
    "utf8",
  );
}

/** 从 Tauri ACP dispatch 调用中提取完整 JSON-RPC 消息。 */
function acpMessageFromCall(call: unknown[]): Record<string, unknown> {
  const args = call[1];
  if (typeof args !== "object" || args === null || Array.isArray(args)) {
    throw new Error("测试桩没有收到 ACP 参数");
  }
  const message = (args as Record<string, unknown>).message;
  if (
    typeof message !== "object" ||
    message === null ||
    Array.isArray(message)
  ) {
    throw new Error("测试桩没有收到 ACP message");
  }
  return message as Record<string, unknown>;
}

/** 安装只接受 acp_dispatch 的真实 JSON-RPC Tauri 桩，并自动完成握手响应。 */
function stubAcpDispatch(
  resultFor: (message: Record<string, unknown>) => unknown,
) {
  type TauriInvoke = (command: string, args?: unknown) => Promise<unknown>;
  const invoke = vi.fn<TauriInvoke>(async (command, args) => {
    if (command !== "acp_dispatch") {
      throw new Error(`ACP 测试不允许调用旧 Tauri 命令：${command}`);
    }
    const message = acpMessageFromCall([command, args]);
    if (message.method === "initialize") {
      return {
        jsonrpc: "2.0",
        id: message.id,
        result: { protocolVersion: 1 },
      };
    }
    return {
      jsonrpc: "2.0",
      id: message.id,
      result: resultFor(message),
    };
  });
  vi.stubGlobal("window", {
    __TAURI_INTERNALS__: { invoke },
  });
  return invoke;
}

describe("后台任务 handler 注册契约", () => {
  it("ACP dispatch 已注册且旧后台与快照命令不再注册", () => {
    const source = readTauriEntrySource();
    const handlerStart = source.indexOf(
      ".invoke_handler(tauri::generate_handler![",
    );
    // 注册宏自身界定列表；桌面 Builder 是否在同一函数中 build 不属于 IPC 契约。
    const handlerEnd = source.indexOf("])", handlerStart);

    expect(handlerStart).toBeGreaterThanOrEqual(0);
    expect(handlerEnd).toBeGreaterThan(handlerStart);

    const handlerSource = source.slice(handlerStart, handlerEnd);
    expect(handlerSource).toContain("acp_host::acp_dispatch");
    for (const handler of [
      "background_tasks_list",
      "background_tasks_cancel_all",
      "session_get_state",
    ]) expect(handlerSource).not.toContain(handler);
  });
});

describe("后台任务 IPC", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("通过 ACP 握手后按明确 Session 查询并返回完整 DTO", async () => {
    const task = backgroundTaskResult();
    const result = {
      sessionId: "session-background",
      tasks: [task],
    };
    const invoke = stubAcpDispatch((message) => {
      expect(message.method).toBe("keencode/background/list");
      expect(message.params).toEqual({ sessionId: "session-background" });
      return result;
    });
    vi.resetModules();
    const { backgroundTasksList: freshBackgroundTasksList } = await import(
      "./api"
    );

    await expect(
      freshBackgroundTasksList("session-background"),
    ).resolves.toEqual([task]);

    expect(invoke).toHaveBeenCalledTimes(2);
    const messages = invoke.mock.calls.map(acpMessageFromCall);
    expect(messages[0]).toMatchObject({
      jsonrpc: "2.0",
      method: "initialize",
      params: {
        protocolVersion: 1,
        clientInfo: { name: "KeenCode", version: "0.0.1" },
        clientCapabilities: { elicitation: { form: {} } },
      },
    });
    expect(messages[0]!.id).toEqual(expect.any(String));
    expect(messages[1]).toEqual({
      jsonrpc: "2.0",
      id: expect.any(String),
      method: "keencode/background/list",
      params: { sessionId: "session-background" },
    });
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain(
      "background_tasks_list",
    );
  });

  it("通过 acp_dispatch 完成握手后取消指定后台任务并返回精确结果", async () => {
    const invoke = stubAcpDispatch((message) => {
      expect(message.method).toBe("keencode/background/cancel");
      expect(message.params).toEqual({
        sessionId: "session-background",
        taskId: "task-shell-1",
      });
      return {
        sessionId: "session-background",
        taskId: "task-shell-1",
        cancelled: true,
      };
    });
    vi.resetModules();
    const { backgroundTaskCancel: freshBackgroundTaskCancel } = await import(
      "./api"
    );

    await expect(
      freshBackgroundTaskCancel("session-background", "task-shell-1"),
    ).resolves.toEqual({
      sessionId: "session-background",
      taskId: "task-shell-1",
      cancelled: true,
    });

    expect(invoke).toHaveBeenCalledTimes(2);
    const initialize = acpMessageFromCall(invoke.mock.calls[0]!);
    expect(initialize).toMatchObject({
      jsonrpc: "2.0",
      method: "initialize",
      params: {
        protocolVersion: 1,
        clientInfo: { name: "KeenCode", version: "0.0.1" },
        clientCapabilities: { elicitation: { form: {} } },
      },
    });
    expect(initialize.id).toEqual(expect.any(String));

    const cancel = acpMessageFromCall(invoke.mock.calls[1]!);
    expect(cancel).toEqual({
      jsonrpc: "2.0",
      id: expect.any(String),
      method: "keencode/background/cancel",
      params: {
        sessionId: "session-background",
        taskId: "task-shell-1",
      },
    });
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain(
      "background_task_cancel",
    );
  });

  it("根 Session 不匹配时拒绝后台任务响应", async () => {
    const invoke = stubAcpDispatch((message) => {
      expect(message.method).toBe("keencode/background/list");
      expect(message.params).toEqual({ sessionId: "session-background" });
      return { sessionId: "session-other", tasks: [] };
    });
    vi.resetModules();
    const { backgroundTasksList: freshBackgroundTasksList } = await import(
      "./api"
    );

    await expect(
      freshBackgroundTasksList("session-background"),
    ).rejects.toThrow("ACP 后台任务响应与请求 Session 不一致");
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain(
      "background_tasks_list",
    );
  });

  it("任务 Session 不匹配时拒绝后台任务响应", async () => {
    const task = {
      ...backgroundTaskResult(),
      sessionId: "session-other",
    };
    const invoke = stubAcpDispatch((message) => {
      expect(message.method).toBe("keencode/background/list");
      expect(message.params).toEqual({ sessionId: "session-background" });
      return { sessionId: "session-background", tasks: [task] };
    });
    vi.resetModules();
    const { backgroundTasksList: freshBackgroundTasksList } = await import(
      "./api"
    );

    await expect(
      freshBackgroundTasksList("session-background"),
    ).rejects.toThrow("ACP 后台任务响应与请求 Session 不一致");
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain(
      "background_tasks_list",
    );
  });

  it("明确 Session 的空后台任务列表可正常返回", async () => {
    const invoke = stubAcpDispatch((message) => {
      expect(message.method).toBe("keencode/background/list");
      expect(message.params).toEqual({ sessionId: "session-background" });
      return { sessionId: "session-background", tasks: [] };
    });
    vi.resetModules();
    const { backgroundTasksList: freshBackgroundTasksList } = await import(
      "./api"
    );

    await expect(
      freshBackgroundTasksList("session-background"),
    ).resolves.toEqual([]);
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain(
      "background_tasks_list",
    );
  });
});

describe("MCP Runtime ACP API", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("通过 keencode/mcp/list 按项目返回启用状态、连接状态、传输类型和可选错误", async () => {
    const snapshot = {
      initPhase: "ready",
      servers: [
        {
          name: "docs",
          enabled: true,
          connectionStatus: "connected",
          transport: "streamable_http",
          toolsCount: 4,
          oauthStatus: "awaiting_authorization",
        },
        {
          name: "local",
          enabled: false,
          connectionStatus: "disabled",
          transport: "stdio",
          toolsCount: 0,
          oauthStatus: "not_required",
        },
        {
          name: "remote",
          enabled: true,
          connectionStatus: "failed",
          transport: "streamable_http",
          toolsCount: 0,
          oauthStatus: "expired",
          error: "连接失败",
        },
      ],
    };
    const invoke = stubAcpDispatch((message) => {
      expect(message.method).toBe("keencode/mcp/list");
      expect(message.params).toEqual({ projectPath: "C:/projects/demo" });
      return snapshot;
    });
    vi.resetModules();
    const { mcpRuntimeList: freshMcpRuntimeList } = await import("./api");

    await expect(freshMcpRuntimeList("C:/projects/demo")).resolves.toEqual(snapshot);
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(acpMessageFromCall(invoke.mock.calls[0]!)).toMatchObject({
      jsonrpc: "2.0",
      method: "initialize",
    });
    expect(acpMessageFromCall(invoke.mock.calls[1]!)).toEqual({
      jsonrpc: "2.0",
      id: expect.any(String),
      method: "keencode/mcp/list",
      params: { projectPath: "C:/projects/demo" },
    });
  });

  it("使用精确的 OAuth ACP 参数并返回 starting、accepted、cancelled 结果", async () => {
    const invoke = stubAcpDispatch((message) => {
      switch (message.method) {
        case "keencode/mcp/oauth_start":
          return { status: "starting" };
        case "keencode/mcp/oauth_callback":
          return { status: "accepted" };
        case "keencode/mcp/oauth_cancel":
          return { cancelled: true };
        default:
          throw new Error(`未预期的 MCP ACP 方法：${String(message.method)}`);
      }
    });
    vi.resetModules();
    const {
      mcpOauthCallback: freshMcpOauthCallback,
      mcpOauthCancel: freshMcpOauthCancel,
      mcpOauthStart: freshMcpOauthStart,
    } = await import("./api");

    await expect(
      freshMcpOauthStart("C:/projects/demo", "docs"),
    ).resolves.toEqual({
      status: "starting",
    });
    await expect(
      freshMcpOauthCallback(
        "C:/projects/demo",
        "docs",
        "code-123",
        "state-456",
      ),
    ).resolves.toEqual({ status: "accepted" });
    await expect(
      freshMcpOauthCancel("C:/projects/demo", "docs"),
    ).resolves.toEqual({
      cancelled: true,
    });

    expect(invoke).toHaveBeenCalledTimes(4);
    const messages = invoke.mock.calls.map(acpMessageFromCall);
    expect(messages.map((message) => message.method)).toEqual([
      "initialize",
      "keencode/mcp/oauth_start",
      "keencode/mcp/oauth_callback",
      "keencode/mcp/oauth_cancel",
    ]);
    expect(messages.slice(1).map((message) => message.params)).toEqual([
      { projectPath: "C:/projects/demo", serverName: "docs" },
      {
        projectPath: "C:/projects/demo",
        serverName: "docs",
        code: "code-123",
        state: "state-456",
      },
      { projectPath: "C:/projects/demo", serverName: "docs" },
    ]);
    expect(messages.slice(1).every((message) => message.jsonrpc === "2.0")).toBe(
      true,
    );
  });
});

describe("个性化设置 IPC", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("通过独立的全局指令命令读取和保存原始文本", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce("使用中文回答")
      .mockResolvedValueOnce("  使用中文回答  ");
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await expect(customInstructionsGet()).resolves.toBe("使用中文回答");
    await expect(customInstructionsSet("  使用中文回答  ")).resolves.toBe(
      "  使用中文回答  ",
    );
    expect(invoke).toHaveBeenNthCalledWith(
      1,
      "custom_instructions_get",
      {},
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      2,
      "custom_instructions_set",
      { instructions: "  使用中文回答  " },
      undefined,
    );
  });

  it("通过独立的长期记忆命令读取和保存 MEMORY.md", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce("# 长期记忆")
      .mockResolvedValueOnce("# 更新后的记忆");
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await expect(memoriesGet()).resolves.toBe("# 长期记忆");
    await expect(memoriesSet("# 更新后的记忆")).resolves.toBe("# 更新后的记忆");
    expect(invoke).toHaveBeenNthCalledWith(1, "memories_get", {}, undefined);
    expect(invoke).toHaveBeenNthCalledWith(
      2,
      "memories_set",
      { content: "# 更新后的记忆" },
      undefined,
    );
  });

  it("按任务 Session ID 查询持久化缓存汇总", async () => {
    const result = {
      sessionId: "session-1",
      requestCount: 2,
      inputTokens: 1_000,
      cacheReadTokens: 100,
      cacheHitRate: 0.1,
      latestContextTokens: 900,
      latestContextEstimated: false,
    };
    const invoke = vi.fn().mockResolvedValue(result);
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await expect(taskCacheUsageGet("session-1")).resolves.toEqual(result);
    expect(invoke).toHaveBeenCalledWith(
      "task_cache_usage_get",
      { sessionId: "session-1" },
      undefined,
    );
  });

  it("Git 提交和推送参数通过类型化 IPC 传递", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce({ commit: "abc1234", branch: "main", output: "ok" })
      .mockResolvedValueOnce({ branch: "main", output: "up to date" });
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await gitCommit({
      projectPath: "/repo",
      message: "Add summary panel",
      includeUnstaged: true,
    });
    await gitPush("/repo");

    expect(invoke).toHaveBeenNthCalledWith(
      1,
      "git_commit",
      {
        projectPath: "/repo",
        message: "Add summary panel",
        includeUnstaged: true,
      },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      2,
      "git_push",
      { projectPath: "/repo" },
      undefined,
    );
  });

  it("按需展开未跟踪目录时传递项目和目录路径", async () => {
    const invoke = vi.fn().mockResolvedValue({ files: [], truncated: false });
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await gitUntrackedDirectory("/repo", "vendor");

    expect(invoke).toHaveBeenCalledWith(
      "git_untracked_directory",
      { projectPath: "/repo", path: "vendor" },
      undefined,
    );
  });

  it("同一项目的并发 Git 状态查询只调用一次 IPC", async () => {
    let resolveStatus!: (value: GitStatusResult) => void;
    const pending = new Promise<GitStatusResult>((resolve) => {
      resolveStatus = resolve;
    });
    const invoke = vi.fn().mockReturnValue(pending);
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    const first = gitStatus("/repo/concurrent", { force: true });
    const second = gitStatus("/repo/concurrent");
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    resolveStatus(gitStatusResult());

    await expect(Promise.all([first, second])).resolves.toEqual([
      gitStatusResult(),
      gitStatusResult(),
    ]);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("每轮查询运行时到达的 force 都合并为紧随其后的一轮刷新", async () => {
    let resolveFirst!: (value: GitStatusResult) => void;
    let resolveSecond!: (value: GitStatusResult) => void;
    let resolveThird!: (value: GitStatusResult) => void;
    const firstPending = new Promise<GitStatusResult>((resolve) => {
      resolveFirst = resolve;
    });
    const secondPending = new Promise<GitStatusResult>((resolve) => {
      resolveSecond = resolve;
    });
    const thirdPending = new Promise<GitStatusResult>((resolve) => {
      resolveThird = resolve;
    });
    const invoke = vi
      .fn()
      .mockReturnValueOnce(firstPending)
      .mockReturnValueOnce(secondPending)
      .mockReturnValueOnce(thirdPending);
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    const first = gitStatus("/repo/queued-force");
    const second = gitStatus("/repo/queued-force", { force: true });
    const sameSecond = gitStatus("/repo/queued-force", { force: true });
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    resolveFirst(gitStatusResult("round-a"));
    await expect(first).resolves.toEqual(gitStatusResult("round-a"));
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));

    const third = gitStatus("/repo/queued-force", { force: true });
    const sameThird = gitStatus("/repo/queued-force", { force: true });
    expect(invoke).toHaveBeenCalledTimes(2);
    resolveSecond(gitStatusResult("round-b"));
    await expect(Promise.all([second, sameSecond])).resolves.toEqual([
      gitStatusResult("round-b"),
      gitStatusResult("round-b"),
    ]);
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(3));
    resolveThird(gitStatusResult("round-c"));

    await expect(Promise.all([third, sameThird])).resolves.toEqual([
      gitStatusResult("round-c"),
      gitStatusResult("round-c"),
    ]);
    expect(invoke).toHaveBeenCalledTimes(3);
  });

  it("Git 状态在 1500 毫秒内复用缓存，force 与过期查询会刷新", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-08-14T00:00:00.000Z"));
    const invoke = vi
      .fn()
      .mockResolvedValueOnce(gitStatusResult("first"))
      .mockResolvedValueOnce(gitStatusResult("forced"))
      .mockResolvedValueOnce(gitStatusResult("expired"));
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await expect(
      gitStatus("/repo/cache", { force: true }),
    ).resolves.toEqual(gitStatusResult("first"));
    await expect(gitStatus("/repo/cache")).resolves.toEqual(
      gitStatusResult("first"),
    );
    expect(invoke).toHaveBeenCalledTimes(1);

    await expect(
      gitStatus("/repo/cache", { force: true }),
    ).resolves.toEqual(gitStatusResult("forced"));
    expect(invoke).toHaveBeenCalledTimes(2);

    vi.advanceTimersByTime(1_500);
    await expect(gitStatus("/repo/cache")).resolves.toEqual(
      gitStatusResult("expired"),
    );
    expect(invoke).toHaveBeenCalledTimes(3);
  });

  it("Git 提交成功后使对应项目的状态缓存失效", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce(gitStatusResult("before"))
      .mockResolvedValueOnce({ commit: "abc1234", branch: "main", output: "ok" })
      .mockResolvedValueOnce(gitStatusResult("after"));
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await gitStatus("/repo/commit", { force: true });
    await gitCommit({
      projectPath: "/repo/commit",
      message: "刷新状态 / Refresh status",
      includeUnstaged: false,
    });
    await expect(gitStatus("/repo/commit")).resolves.toEqual(
      gitStatusResult("after"),
    );

    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "git_status",
      "git_commit",
      "git_status",
    ]);
  });

  it("项目内文件写入成功后使对应 Git 状态缓存失效", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce(gitStatusResult("before-write"))
      .mockResolvedValueOnce(fsWriteResult("/repo/write-file/src/file.ts"))
      .mockResolvedValueOnce(gitStatusResult("after-write"));
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await gitStatus("/repo/write-file", { force: true });
    await fsWriteFile(
      "/repo/write-file",
      "src/file.ts",
      "export const value = 1;",
      1_723_600_000_000,
    );
    await expect(gitStatus("/repo/write-file")).resolves.toEqual(
      gitStatusResult("after-write"),
    );

    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "git_status",
      "fs_write_file",
      "git_status",
    ]);
  });

  it("绝对路径写入成功后按 Windows 路径定位并失效项目缓存", async () => {
    const projectPath = "D:\\repo\\write-absolute";
    const absolutePath = "\\\\?\\D:\\repo\\write-absolute\\src\\file.ts";
    const invoke = vi
      .fn()
      .mockResolvedValueOnce(gitStatusResult("before-write"))
      .mockResolvedValueOnce(fsWriteResult(absolutePath))
      .mockResolvedValueOnce(gitStatusResult("after-write"));
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await gitStatus(projectPath, { force: true });
    await fsWriteAbsolute(
      "D:\\repo\\write-absolute\\src\\file.ts",
      "export const value = 2;",
    );
    await expect(gitStatus(projectPath)).resolves.toEqual(
      gitStatusResult("after-write"),
    );

    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      "git_status",
      "fs_write_absolute",
      "git_status",
    ]);
  });
});

describe("应用设置 IPC", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("读取并保存兼容服务 URL 使用当前设置字段往返", async () => {
    const saved = {
      interfaceLanguage: "zh",
      appUpdateDownloadSource: "auto",
      chromeHardwareAcceleration: true,
      sidebarCollapsedProjectIds: [],
      projectDirectory: "D:/projects",
      taskNotifications: true,
      notificationSound: true,
      keepComputerAwake: true,
      backgroundAgentLimit: 10,
      terminalFontFamily: "monospace",
      terminalShell: "auto",
      localMemories: true,
      autoArchiveConversations: true,
      archiveRetentionDays: 7,
      webServiceUrl: "http://127.0.0.1:3456/compat",
    } as const;
    const invoke = vi.fn().mockResolvedValue(saved);
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await expect(settingsGet()).resolves.toEqual(saved);
    await expect(
      settingsSet({ webServiceUrl: saved.webServiceUrl }),
    ).resolves.toEqual(saved);
    expect(invoke).toHaveBeenNthCalledWith(
      1,
      "settings_get",
      {},
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      2,
      "settings_set",
      { settings: { webServiceUrl: saved.webServiceUrl } },
      undefined,
    );
  });

  it("保存空字符串作为明确的网络工具禁用值", async () => {
    const invoke = vi.fn().mockResolvedValue({ webServiceUrl: "" });
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await expect(settingsSet({ webServiceUrl: "" })).resolves.toEqual({
      webServiceUrl: "",
    });
    expect(invoke).toHaveBeenCalledWith(
      "settings_set",
      { settings: { webServiceUrl: "" } },
      undefined,
    );
  });
});

describe("项目范围扩展查询 IPC", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("把当前项目路径传给插件、MCP 和子智能体查询", async () => {
    const invoke = vi.fn().mockResolvedValue({});
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await inspectMcp("D:/projects/active");
    await pluginsList("D:/projects/active");
    await mcpDoctor("docs", "D:/projects/active");
    await agentsList("D:/projects/active");
    await agentDetail("reviewer", "D:/projects/active");

    expect(invoke).toHaveBeenNthCalledWith(
      1,
      "inspect_mcp",
      { projectPath: "D:/projects/active" },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      2,
      "plugins_list",
      { projectPath: "D:/projects/active" },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      3,
      "mcp_doctor",
      { focus: "docs", projectPath: "D:/projects/active" },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      4,
      "agents_list",
      { projectPath: "D:/projects/active" },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      5,
      "agent_detail",
      { name: "reviewer", projectPath: "D:/projects/active" },
      undefined,
    );
  });

  it("无项目上下文时明确发送 null，使用全局扩展视图", async () => {
    const invoke = vi.fn().mockResolvedValue({});
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await inspectMcp();
    await pluginsList(null);
    await mcpDoctor(null, null);
    await agentsList(null);
    await agentDetail("plan", null);

    expect(invoke).toHaveBeenNthCalledWith(
      1,
      "inspect_mcp",
      { projectPath: null },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      2,
      "plugins_list",
      { projectPath: null },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      3,
      "mcp_doctor",
      { focus: null, projectPath: null },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      4,
      "agents_list",
      { projectPath: null },
      undefined,
    );
    expect(invoke).toHaveBeenNthCalledWith(
      5,
      "agent_detail",
      { name: "plan", projectPath: null },
      undefined,
    );
  });
});

describe("请求历史 IPC", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("把筛选和分页字段作为一个查询对象发送", async () => {
    const invoke = vi.fn().mockResolvedValue({
      records: [],
      total: 0,
      offset: 20,
      limit: 20,
      hasMore: false,
      models: [],
      statuses: [],
    });
    vi.stubGlobal("window", { __TAURI_INTERNALS__: { invoke } });
    const query = {
      offset: 20,
      limit: 20,
      model: "example-model",
      status: "completed",
      fromMs: 100,
      toMs: 200,
    };

    await requestRecordsList(query);

    expect(invoke).toHaveBeenCalledWith(
      "request_records_list",
      { query },
      undefined,
    );
  });
});

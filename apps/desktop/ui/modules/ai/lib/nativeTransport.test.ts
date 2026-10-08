import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { readUIMessageStream, type UIMessage } from "ai";
import type { NativeAgentEvent } from "@/modules/ai/lib/nativeStream";
import { runNativeAgentStream } from "@/modules/ai/lib/nativeTransport";
import { respondToToolApproval } from "@/modules/ai/lib/nativeApproval";
import { EMPTY_PROVIDER_KEYS } from "@/modules/ai/lib/keyring";
import { BUILTIN_AGENTS } from "@/modules/ai/lib/agents";
import { DEFAULT_AGENT_INSTRUCTIONS } from "@/modules/ai/lib/agentInstructions";
import type { ToolContext } from "@/modules/ai/tools/context";
import { EMPTY_SUBAGENT_CONFIG } from "@/modules/ai/agents/config";
import {
  conversationTool,
  conversationWorkState,
} from "@/modules/ai/lib/conversationPresentation";
import {
  CLIENT_TOOL_RESULT_MAX_BYTES,
  serializedResultBytes,
} from "@/modules/ai/lib/clientToolResult";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: (event: unknown) => void = () => {};
  },
}));
vi.mock("@/modules/ai/store/subagentsStore", () => ({
  useSubagentsStore: {
    getState: () => ({
      config: EMPTY_SUBAGENT_CONFIG,
      hydrate: async () => {},
    }),
  },
}));

const context: ToolContext = {
  getCwd: () => "D:/workspace",
  getWorkspaceRoot: () => "D:/workspace",
  getSessionId: () => "session-1",
  getTerminalContext: () => null,
  isActiveTerminalPrivate: () => false,
  injectIntoActivePty: () => false,
  openPreview: () => false,
  spawnAgent: () => null,
  readAgentOutput: () => null,
  readCache: new Map(),
};
const model = {
  protocol: "chat_completions" as const,
  providerId: "openai-compatible",
  baseUrl: "http://127.0.0.1:1234/v1",
  model: "model",
  contextLimit: 128_000,
  secretAccount: "compat-local-api-key",
  allowPrivateNetwork: true,
};

vi.mock("@/modules/ai/lib/businessTools", () => ({
  businessConfiguration: async () => ({ todos: [], subagents: [] }),
  projectRuntimeTodos: vi.fn(),
}));
beforeEach(() => vi.mocked(invoke).mockReset());

describe("native transport IPC lifecycle", () => {
  it("bounds oversized project memory results before IPC", async () => {
    let onEvent!: { onmessage: (event: NativeAgentEvent) => void };
    let submitted: unknown;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start")
        onEvent = (args as { onEvent: typeof onEvent }).onEvent;
      if (command === "agent_memory_read")
        return { content: "界".repeat(600_000) };
      if (command === "agent_core_tool_result") {
        submitted = (args as { result: unknown }).result;
        onEvent.onmessage({ type: "end" });
      }
    });
    const stream = await runNativeAgentStream(
      {
        keys: EMPTY_PROVIDER_KEYS,
        uiMessages: [],
        toolContext: context,
        memory: { id: "fixture", index: "" },
      },
      model,
    );
    onEvent.onmessage({
      type: "client_tool",
      id: "memory",
      name: "memory_read",
      input: {},
    });
    const reader = stream.getReader();
    while (!(await reader.read()).done) {}
    expect(serializedResultBytes(submitted)).toBeLessThanOrEqual(
      CLIENT_TOOL_RESULT_MAX_BYTES,
    );
    expect(submitted).toMatchObject({ truncated: true });
  });

  it.each([false, true])(
    "handles rejected tool results with closed=%s",
    async (closed) => {
      let onEvent!: { onmessage: (event: NativeAgentEvent) => void };
      const results: unknown[] = [];
      vi.mocked(invoke).mockImplementation(async (command, args) => {
        if (command === "agent_core_start")
          onEvent = (args as { onEvent: typeof onEvent }).onEvent;
        if (command === "agent_core_tool_result") {
          results.push((args as { result: unknown }).result);
          if (closed || results.length === 1)
            throw new Error("receiver closed");
          onEvent.onmessage({ type: "end" });
        }
      });
      const stream = await runNativeAgentStream(
        { keys: EMPTY_PROVIDER_KEYS, toolContext: context, uiMessages: [] },
        model,
      );
      onEvent.onmessage({
        type: "client_tool",
        id: "terminal",
        name: "get_terminal_output",
        input: {},
      });
      const reader = stream.getReader();
      const chunks = [];
      for (;;) {
        const next = await reader.read();
        if (next.done) break;
        chunks.push(next.value);
      }
      expect(results).toHaveLength(2);
      expect(results[1]).toEqual({ error: "receiver closed" });
      if (closed) {
        expect(invoke).toHaveBeenCalledWith(
          "agent_core_cancel",
          expect.anything(),
        );
        expect(chunks).toContainEqual({
          type: "error",
          errorText: "receiver closed",
        });
      } else
        expect(invoke).not.toHaveBeenCalledWith(
          "agent_core_cancel",
          expect.anything(),
        );
    },
  );

  it("keeps business tools off the client callback list and sends permissions", async () => {
    let request!: Record<string, unknown>;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start") {
        const call = args as {
          request: typeof request;
          onEvent: { onmessage: (event: NativeAgentEvent) => void };
        };
        request = call.request;
        call.onEvent.onmessage({ type: "end" });
      }
    });
    await runNativeAgentStream(
      {
        keys: EMPTY_PROVIDER_KEYS,
        toolContext: context,
        uiMessages: [],
        permissionMode: "full-access",
        planMode: true,
      },
      model,
    );
    expect(request).toMatchObject({
      permissionMode: "full-access",
      planMode: true,
      todos: [],
      subagents: [],
    });
    const names = (request.clientTools as { name: string }[]).map(
      (tool) => tool.name,
    );
    for (const name of [
      "list_directory",
      "todo_write",
      "run_subagent",
      "bash_background",
      "bash_logs",
      "bash_list",
      "bash_kill",
    ])
      expect(names).not.toContain(name);
  });
  it("sends the complete role, then global rules, then project rules to Rust", async () => {
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start") {
        const { request, onEvent } = args as {
          request: { system: string };
          onEvent: { onmessage: (event: NativeAgentEvent) => void };
        };
        expect(request.system.split(DEFAULT_AGENT_INSTRUCTIONS)).toHaveLength(
          2,
        );
        expect(
          request.system.indexOf(BUILTIN_AGENTS[1].instructions),
        ).toBeLessThan(request.system.indexOf("GLOBAL_RULE"));
        expect(request.system.indexOf("Use native Read")).toBeLessThan(
          request.system.indexOf("GLOBAL_RULE"),
        );
        expect(request.system.indexOf("GLOBAL_RULE")).toBeLessThan(
          request.system.indexOf("PROJECT_RULE"),
        );
        expect(request.system.endsWith("PROJECT_RULE")).toBe(true);
        onEvent.onmessage({ type: "end" });
      }
      return undefined;
    });
    await runNativeAgentStream(
      {
        keys: EMPTY_PROVIDER_KEYS,
        toolContext: context,
        uiMessages: [],
        agentPersona: BUILTIN_AGENTS[1],
        globalInstructions: "GLOBAL_RULE",
        projectInstructions: "PROJECT_RULE",
      },
      model,
    );
    expect(invoke).toHaveBeenCalledWith("agent_core_start", expect.anything());
  });

  it("preserves cancellation metadata through the SDK UI-message parser", async () => {
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start") {
        const { onEvent } = args as {
          onEvent: { onmessage: (event: NativeAgentEvent) => void };
        };
        for (const event of [
          {
            type: "tool_input",
            id: "cancel",
            name: "Write",
            input: { file_path: "cancelled.txt" },
          },
          {
            type: "approval",
            id: "rcode:run:cancel",
            toolCallId: "cancel",
            name: "Write",
            input: {},
          },
          { type: "finish", cancelled: true },
          { type: "end" },
        ] satisfies NativeAgentEvent[])
          onEvent.onmessage(event);
      }
      return undefined;
    });
    const stream = await runNativeAgentStream(
      { keys: EMPTY_PROVIDER_KEYS, toolContext: context, uiMessages: [] },
      model,
    );
    let result: UIMessage | undefined;
    for await (const message of readUIMessageStream({ stream }))
      result = message;
    const part = result?.parts.find((part) => part.type === "tool-write_file");
    const cancelled = part && conversationTool(part);
    if (!cancelled) throw new Error("SDK 未保留待审批工具");
    expect(conversationWorkState(cancelled)).toBe("cancelled");
  });

  it("projects URL schemas for Rust while rejecting invalid client-tool URLs before execution", async () => {
    const openPreview = vi.fn();
    let onEvent!: { onmessage: (event: NativeAgentEvent) => void };
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start") {
        const start = args as {
          request: {
            clientTools: {
              name: string;
              inputSchema: { properties: { url?: Record<string, unknown> } };
            }[];
          };
          onEvent: typeof onEvent;
        };
        const preview = start.request.clientTools.find(
          (tool) => tool.name === "open_preview",
        );
        expect(preview?.inputSchema.properties.url).toMatchObject({
          type: "string",
        });
        expect(preview?.inputSchema.properties.url).not.toHaveProperty(
          "format",
        );
        onEvent = start.onEvent;
        onEvent.onmessage({
          type: "client_tool",
          id: "rcode:run:invalid-url",
          name: "open_preview",
          input: { url: "invalid-url" },
        });
      } else if (command === "agent_core_tool_result") {
        expect(args).toMatchObject({
          requestId: "rcode:run:invalid-url",
          result: { error: expect.any(String) },
        });
        onEvent.onmessage({ type: "end" });
      }
      return undefined;
    });
    const stream = await runNativeAgentStream(
      {
        keys: EMPTY_PROVIDER_KEYS,
        toolContext: { ...context, openPreview },
        uiMessages: [],
      },
      model,
    );
    for await (const _message of readUIMessageStream({ stream })) {
      // 完整消费到核心结束事件，避免仅验证异步工具启动。
    }
    expect(invoke).toHaveBeenCalledWith(
      "agent_core_tool_result",
      expect.objectContaining({ requestId: "rcode:run:invalid-url" }),
    );
    expect(openPreview).not.toHaveBeenCalled();
  });

  it("consumes a denied Rust approval through the actual SDK UI-message parser", async () => {
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start") {
        const { onEvent } = args as {
          onEvent: { onmessage: (event: NativeAgentEvent) => void };
        };
        for (const event of [
          { type: "model", round: 1, event: { type: "message_start" } },
          {
            type: "tool_input",
            id: "c1",
            name: "Write",
            input: { file_path: "proof.txt", content: "proof" },
          },
          {
            type: "approval",
            id: "rcode:run:c1",
            toolCallId: "c1",
            name: "Write",
            input: {},
          },
          {
            type: "tool_result",
            result: {
              toolCallId: "c1",
              isError: true,
              content: [{ type: "text", text: "用户拒绝" }],
            },
          },
          { type: "model", round: 2, event: { type: "message_start" } },
          {
            type: "model",
            round: 2,
            event: { type: "text_delta", index: 0, delta: "已停止写入" },
          },
          { type: "finish", cancelled: false },
          { type: "end" },
        ] satisfies NativeAgentEvent[])
          onEvent.onmessage(event);
      }
      return undefined;
    });
    const stream = await runNativeAgentStream(
      { keys: EMPTY_PROVIDER_KEYS, toolContext: context, uiMessages: [] },
      model,
    );
    const messages = [];
    for await (const message of readUIMessageStream({
      stream,
      terminateOnError: true,
    }))
      messages.push(message);
    const last = messages[messages.length - 1];
    expect(last.parts).toContainEqual(
      expect.objectContaining({
        type: "tool-write_file",
        state: "output-error",
        errorText: "用户拒绝",
      }),
    );
    expect(last.parts).toContainEqual(
      expect.objectContaining({ type: "text", text: "已停止写入" }),
    );
  });
  it("cancels again after start ACK when abort happens before backend registration", async () => {
    const abort = new AbortController();
    let request: Record<string, unknown> = {};
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start") {
        request = (args as { request: Record<string, unknown> }).request;
        abort.abort();
      }
      return undefined;
    });
    const stream = await runNativeAgentStream(
      {
        keys: { ...EMPTY_PROVIDER_KEYS, openai: "must-not-cross-ipc" },
        modelId: "gpt-5.6",
        toolContext: context,
        uiMessages: [],
        abortSignal: abort.signal,
      },
      model,
    );
    expect(invoke).toHaveBeenCalledWith("agent_core_cancel", {
      runId: request.runId,
    });
    expect(JSON.stringify(request)).not.toContain("must-not-cross-ipc");
    expect(request.secretAccount).toBe("compat-local-api-key");
    const names = (request.clientTools as { name: string }[]).map(
      (tool) => tool.name,
    );
    expect(names).not.toContain("bash_run");
    expect(names).not.toContain("bash_background");
    await stream.cancel();
  });
  it("native approvals resume the existing runner without calling SDK auto-send", async () => {
    vi.mocked(invoke).mockResolvedValue(undefined);
    const sdk = vi.fn();
    await respondToToolApproval("rcode:run:c1", true, sdk);
    expect(sdk).not.toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledWith("agent_core_approve", {
      approvalId: "rcode:run:c1",
      approved: true,
    });
    respondToToolApproval("sdk-approval", false, sdk);
    expect(sdk).toHaveBeenCalledWith({ id: "sdk-approval", approved: false });
  });
});

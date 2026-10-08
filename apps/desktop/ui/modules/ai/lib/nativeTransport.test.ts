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
import {
  EMPTY_SUBAGENT_CONFIG,
  configuredSubagents,
} from "@/modules/ai/agents/config";
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

beforeEach(() => vi.mocked(invoke).mockReset());

describe("native transport IPC lifecycle", () => {
  it("bounds oversized client results before IPC and preserves log pagination", async () => {
    let onEvent!: { onmessage: (event: NativeAgentEvent) => void };
    let submitted: unknown;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start")
        onEvent = (args as { onEvent: typeof onEvent }).onEvent;
      if (command === "shell_bg_logs") {
        expect(args).toMatchObject({ maxBytes: 64 * 1024, sinceOffset: 0 });
        return {
          bytes: "\u0000界".repeat(300_000),
          next_offset: 1_200_000,
          has_more: true,
          dropped: 0,
        };
      }
      if (command === "agent_core_tool_result") {
        submitted = (args as { result: unknown }).result;
        onEvent.onmessage({ type: "end" });
      }
      return undefined;
    });
    const stream = await runNativeAgentStream(
      { keys: EMPTY_PROVIDER_KEYS, toolContext: context, uiMessages: [] },
      model,
    );
    onEvent.onmessage({
      type: "client_tool",
      id: "rcode:run:large-logs",
      name: "bash_logs",
      input: { handle: 7, since_offset: 0 },
    });
    const reader = stream.getReader();
    while (!(await reader.read()).done) {}
    expect(submitted).toMatchObject({
      truncated: true,
      warning: expect.stringContaining("incomplete"),
    });
    expect(serializedResultBytes(submitted)).toBeLessThanOrEqual(
      CLIENT_TOOL_RESULT_MAX_BYTES,
    );
    expect(invoke).not.toHaveBeenCalledWith("agent_core_cancel", expect.anything());
  });

  it("returns a small explicit error when submitting a tool result is rejected", async () => {
    let onEvent!: { onmessage: (event: NativeAgentEvent) => void };
    const results: unknown[] = [];
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start")
        onEvent = (args as { onEvent: typeof onEvent }).onEvent;
      if (command === "shell_bg_logs")
        return { bytes: "log", next_offset: 3, has_more: false, dropped: 0 };
      if (command === "agent_core_tool_result") {
        results.push((args as { result: unknown }).result);
        if (results.length === 1) throw new Error("IPC rejected result");
        onEvent.onmessage({ type: "end" });
      }
      return undefined;
    });
    const stream = await runNativeAgentStream(
      { keys: EMPTY_PROVIDER_KEYS, toolContext: context, uiMessages: [] },
      model,
    );
    onEvent.onmessage({
      type: "client_tool",
      id: "rcode:run:logs",
      name: "bash_logs",
      input: { handle: 7 },
    });
    const reader = stream.getReader();
    while (!(await reader.read()).done) {}
    expect(results).toHaveLength(2);
    expect(results[1]).toEqual({ error: "IPC rejected result" });
    expect(serializedResultBytes(results[1])).toBeLessThan(4096);
    expect(invoke).not.toHaveBeenCalledWith("agent_core_cancel", expect.anything());
  });

  it("cancels the run and exposes the reason when neither result submission succeeds", async () => {
    let onEvent!: { onmessage: (event: NativeAgentEvent) => void };
    let attempts = 0;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start")
        onEvent = (args as { onEvent: typeof onEvent }).onEvent;
      if (command === "shell_bg_logs")
        return { bytes: "log", next_offset: 3, has_more: false, dropped: 0 };
      if (command === "agent_core_tool_result") {
        attempts++;
        throw new Error("receiver closed");
      }
      return undefined;
    });
    const stream = await runNativeAgentStream(
      { keys: EMPTY_PROVIDER_KEYS, toolContext: context, uiMessages: [] },
      model,
    );
    onEvent.onmessage({
      type: "client_tool",
      id: "rcode:run:closed",
      name: "bash_logs",
      input: { handle: 7 },
    });
    const chunks = [];
    const reader = stream.getReader();
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) break;
      chunks.push(chunk.value);
    }
    expect(attempts).toBe(2);
    expect(invoke).toHaveBeenCalledWith("agent_core_cancel", expect.anything());
    expect(chunks).toContainEqual({
      type: "error",
      errorText: "receiver closed",
    });
  });

  it("freezes a custom child's prompt and exact tools without exposing delegation or shell", async () => {
    const child = {
      ...configuredSubagents(EMPTY_SUBAGENT_CONFIG)[0],
      id: "custom-test",
      builtIn: false,
      systemPrompt: "CUSTOM_RULE",
      tools: ["grep"],
      injectAgentsMd: true,
    };
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start") {
        const { request, onEvent } = args as {
          request: Record<string, unknown>;
          onEvent: { onmessage: (event: NativeAgentEvent) => void };
        };
        expect(request).toMatchObject({
          planMode: true,
          permissionMode: "ask",
          subagentTools: ["Grep"],
          clientTools: [],
          system: "CUSTOM_RULE\n\nGLOBAL_RULE\n\nPROJECT_RULE",
        });
        onEvent.onmessage({ type: "end" });
      }
      return undefined;
    });
    await runNativeAgentStream(
      {
        keys: EMPTY_PROVIDER_KEYS,
        toolContext: context,
        uiMessages: [],
        memory: { id: "scope", index: "PRIVATE_MEMORY" },
        globalInstructions: "GLOBAL_RULE",
        projectInstructions: "PROJECT_RULE",
      },
      model,
      child,
    );
  });
  it("passes selected permissions over IPC and keeps child runs read-only", async () => {
    const requests: Record<string, unknown>[] = [];
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "agent_core_start") {
        const { request, onEvent } = args as {
          request: Record<string, unknown>;
          onEvent: { onmessage: (event: NativeAgentEvent) => void };
        };
        requests.push(request);
        onEvent.onmessage({ type: "end" });
      }
      return undefined;
    });
    const opts = {
      keys: EMPTY_PROVIDER_KEYS,
      toolContext: context,
      uiMessages: [],
      permissionMode: "full-access" as const,
      planMode: true,
    };
    await runNativeAgentStream(opts, model);
    await runNativeAgentStream({ ...opts, planMode: false }, model, "explore");
    expect(requests[0]).toEqual(
      expect.objectContaining({
        permissionMode: "full-access",
        planMode: true,
      }),
    );
    expect(requests[1]).toEqual(
      expect.objectContaining({ permissionMode: "ask", planMode: true }),
    );
    expect(
      (requests[1].clientTools as { name: string }[]).map((tool) => tool.name),
    ).toEqual(["list_directory"]);
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
    expect(names).toContain("bash_background");
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

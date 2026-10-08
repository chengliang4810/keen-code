import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ToolCallOptions, UIMessageChunk } from "ai";
import { EMPTY_PROVIDER_KEYS } from "@/modules/ai/lib/keyring";
import { EMPTY_SUBAGENT_CONFIG } from "@/modules/ai/agents/config";
import type { RunAgentOptions } from "@/modules/ai/lib/agent";
import type { ToolContext } from "@/modules/ai/tools/context";
import type { NativeAgentEvent } from "@/modules/ai/lib/nativeStream";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  hydrate: vi.fn(),
  setTodos: vi.fn(),
  todos: {} as Record<string, unknown[]>,
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
  Channel: class {
    onmessage: (event: NativeAgentEvent) => void = () => {};
  },
}));
vi.mock("@/modules/ai/store/subagentsStore", () => ({
  useSubagentsStore: {
    getState: () => ({ config: EMPTY_SUBAGENT_CONFIG, hydrate: mocks.hydrate }),
  },
}));
vi.mock("@/modules/ai/store/todoStore", () => ({
  useTodosStore: {
    getState: () => ({
      bySession: mocks.todos,
      hydrate: mocks.hydrate,
      setTodos: mocks.setTodos,
    }),
  },
}));
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => ({ kind: "wsl", distro: "Ubuntu" }),
}));
import {
  businessConfiguration,
  prepareBusinessBridge,
  projectRuntimeTodos,
} from "@/modules/ai/lib/businessTools";

const context: ToolContext = {
  getCwd: () => "/workspace",
  getWorkspaceRoot: () => "/workspace",
  getSessionId: () => "session",
  getTerminalContext: () => null,
  isActiveTerminalPrivate: () => false,
  injectIntoActivePty: () => false,
  openPreview: () => false,
  spawnAgent: () => null,
  readAgentOutput: () => null,
  readCache: new Map(),
};
const opts: RunAgentOptions = {
  keys: EMPTY_PROVIDER_KEYS,
  modelId: "openai-compatible-custom",
  openaiCompatibleModelId: "fixture",
  openaiCompatibleBaseURL: "http://127.0.0.1:1234/v1",
  toolContext: context,
  uiMessages: [],
  permissionMode: "edit",
};
const definition = {
  name: "bash_background",
  description: "shared tool",
  inputSchema: {
    type: "object",
    properties: { command: { type: "string" } },
    required: ["command"],
    additionalProperties: false,
  },
};
let onEvent!: { onmessage: (event: NativeAgentEvent) => void };
beforeEach(() => {
  vi.resetAllMocks();
  mocks.todos = {};
  mocks.hydrate.mockResolvedValue(undefined);
  mocks.invoke.mockImplementation(async (name, args) => {
    if (name === "agent_business_start") {
      onEvent = args.onEvent;
      return [definition];
    }
    if (name === "agent_business_execute")
      return { ok: true, handle: "task-shared" };
  });
});

describe("shared business UI adapter", () => {
  it("seeds canonical Todo data and sends subagent model references without credentials", async () => {
    mocks.todos.session = [
      {
        id: "display-id",
        title: "inspect",
        description: "inspecting",
        status: "in_progress",
      },
    ];
    const configuration = await businessConfiguration(opts);
    expect(configuration.todos).toEqual([
      { content: "inspect", active_form: "inspecting", status: "in_progress" },
    ]);
    expect(configuration.subagents).toHaveLength(4);
    expect(configuration.subagents[0]).toMatchObject({
      template: { id: "explore" },
      model: { model: "fixture" },
      secretAccount: "openai-compatible-api-key",
    });
    expect(configuration).not.toHaveProperty("keys");
    projectRuntimeTodos("session", [
      { content: "inspect", active_form: "inspecting", status: "completed" },
    ]);
    expect(mocks.setTodos).toHaveBeenCalledWith("session", [
      {
        id: "inspect",
        title: "inspect",
        description: "inspecting",
        status: "completed",
      },
    ]);
  });
  it("uses Rust schemas and execution while passing permission and WSL context", async () => {
    const bridge = await prepareBusinessBridge(opts);
    expect(mocks.invoke).toHaveBeenCalledWith(
      "agent_business_start",
      expect.objectContaining({
        workspace: { kind: "wsl", distro: "Ubuntu" },
        request: expect.objectContaining({
          cwd: "/workspace",
          permissionMode: "edit",
          planMode: false,
        }),
      }),
    );
    expect(bridge.tools.bash_background.needsApproval).toBeUndefined();
    const execute = bridge.tools.bash_background.execute;
    if (!execute) throw new Error("missing bridge executor");
    const input = { command: "pnpm dev" };
    expect(
      await execute(input, {
        toolCallId: "call",
        messages: [],
      } satisfies ToolCallOptions),
    ).toEqual({ ok: true, handle: "task-shared" });
    expect(mocks.invoke).toHaveBeenCalledWith(
      "agent_business_execute",
      expect.objectContaining({
        name: "bash_background",
        callId: "call",
        input,
      }),
    );
    await bridge.dispose();
    await bridge.dispose();
    expect(
      mocks.invoke.mock.calls.filter(
        ([name]) => name === "agent_business_finish",
      ),
    ).toHaveLength(1);
  });
  it("orders Rust approval after the SDK input and releases the run after stream completion", async () => {
    const bridge = await prepareBusinessBridge(opts);
    onEvent.onmessage({
      type: "approval",
      id: "rcode:run:call",
      toolCallId: "call",
      name: "bash_background",
      input: { command: "dev" },
    });
    const stream = new ReadableStream<UIMessageChunk>({
      start(controller) {
        controller.enqueue({ type: "start", messageId: "message" });
        controller.enqueue({
          type: "tool-input-available",
          toolCallId: "call",
          toolName: "bash_background",
          input: { command: "dev" },
        });
        controller.enqueue({
          type: "tool-output-available",
          toolCallId: "call",
          output: { handle: "task" },
        });
        controller.enqueue({ type: "finish" });
        controller.close();
      },
    });
    const chunks: UIMessageChunk[] = [];
    const reader = bridge.wrap(stream).getReader();
    for (;;) {
      const next = await reader.read();
      if (next.done) break;
      chunks.push(next.value);
    }
    expect(chunks.map((chunk) => chunk.type)).toEqual([
      "start",
      "tool-input-available",
      "tool-approval-request",
      "tool-output-available",
      "finish",
    ]);
    await vi.waitFor(() =>
      expect(mocks.invoke).toHaveBeenCalledWith(
        "agent_business_finish",
        expect.anything(),
      ),
    );
  });
  it.each(["finish", "abort", "close"] as const)(
    "waits for the shared finish ACK before publishing %s",
    async (ending) => {
      let acknowledge!: () => void;
      const finish = new Promise<void>((resolve) => {
        acknowledge = resolve;
      });
      mocks.invoke.mockImplementation(async (name, args) => {
        if (name === "agent_business_start") {
          onEvent = args.onEvent;
          return [definition];
        }
        if (name === "agent_business_finish") return finish;
      });
      const bridge = await prepareBusinessBridge(opts);
      const disposing = bridge.dispose();
      const source = new ReadableStream<UIMessageChunk>({
        start(controller) {
          if (ending !== "close") controller.enqueue({ type: ending });
          controller.close();
        },
      });
      const reader = bridge.wrap(source).getReader();
      let published = false;
      const reading = reader.read().then((result) => {
        published = true;
        return result;
      });
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(published).toBe(false);
      acknowledge();
      await disposing;
      const result = await reading;
      if (ending === "close") expect(result.done).toBe(true);
      else {
        expect(result.value?.type).toBe(ending);
        expect((await reader.read()).done).toBe(true);
      }
      expect(
        mocks.invoke.mock.calls.filter(
          ([name]) => name === "agent_business_finish",
        ),
      ).toHaveLength(1);
    },
  );
  it("releases the Rust run before publishing a source failure", async () => {
    let acknowledge!: () => void;
    const finish = new Promise<void>((resolve) => {
      acknowledge = resolve;
    });
    mocks.invoke.mockImplementation(async (name) => {
      if (name === "agent_business_start") return [definition];
      if (name === "agent_business_finish") return finish;
    });
    const bridge = await prepareBusinessBridge(opts);
    const failure = new Error("source failed");
    const reader = bridge
      .wrap(
        new ReadableStream<UIMessageChunk>({
          start(controller) {
            controller.error(failure);
          },
        }),
      )
      .getReader();
    let published = false;
    const reading = reader.read().catch((error: unknown) => {
      published = true;
      return error;
    });
    await vi.waitFor(() =>
      expect(mocks.invoke).toHaveBeenCalledWith(
        "agent_business_finish",
        expect.anything(),
      ),
    );
    expect(published).toBe(false);
    acknowledge();
    expect(await reading).toBe(failure);
  });
  it("propagates an abort during start ACK and stream cancellation to Rust", async () => {
    const abort = new AbortController();
    mocks.invoke.mockImplementation(async (name, args) => {
      if (name === "agent_business_start") {
        onEvent = args.onEvent;
        abort.abort();
        return [definition];
      }
    });
    const bridge = await prepareBusinessBridge({
      ...opts,
      abortSignal: abort.signal,
    });
    expect(mocks.invoke).toHaveBeenCalledWith(
      "agent_core_cancel",
      expect.anything(),
    );
    const cancel = vi.fn();
    const stream = bridge.wrap(new ReadableStream({ cancel }));
    await stream.cancel();
    expect(cancel).toHaveBeenCalled();
    expect(mocks.invoke).toHaveBeenCalledWith(
      "agent_business_finish",
      expect.anything(),
    );
  });
});

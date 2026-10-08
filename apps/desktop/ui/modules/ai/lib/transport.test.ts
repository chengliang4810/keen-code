import { usePreferencesStore } from "@/modules/settings/preferences";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { EMPTY_PROVIDER_KEYS } from "@/modules/ai/lib/keyring";
import type { ToolContext } from "@/modules/ai/tools/context";
import type { PermissionMode } from "@/modules/ai/lib/permissions";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  sdk: vi.fn(),
  rust: vi.fn(),
  nativeModel: vi.fn(),
  business: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@/modules/ai/lib/agent", () => ({ runAgentStream: mocks.sdk }));
vi.mock("@/modules/ai/lib/nativeTransport", () => ({
  runNativeAgentStream: mocks.rust,
}));
vi.mock("@/modules/ai/lib/businessTools", () => ({
  prepareBusinessBridge: mocks.business,
}));
vi.mock("@/modules/ai/lib/nativeProtocol", () => ({
  resolveNativeModel: mocks.nativeModel,
}));
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => ({ kind: "local" }),
  LOCAL_WORKSPACE: { kind: "local" },
}));

import { createContextAwareTransport } from "@/modules/ai/lib/transport";

const context: ToolContext = {
  getCwd: () => "D:/project",
  getWorkspaceRoot: () => "D:/project",
  getSessionId: () => "session",
  getTerminalContext: () => null,
  isActiveTerminalPrivate: () => false,
  injectIntoActivePty: () => false,
  openPreview: () => false,
  spawnAgent: () => null,
  readAgentOutput: () => null,
  readCache: new Map(),
};

function transport(
  root: string | null = "D:/project",
  prepareWorkspace?: () => Promise<void>,
) {
  return createContextAwareTransport({
    prepareWorkspace,
    getKeys: () => EMPTY_PROVIDER_KEYS,
    toolContext: context,
    getModelId: () => "gpt-5.6",
    getAgentPersona: () => ({ name: "Coder", instructions: "ROLE_RULE" }),
    getLive: () => ({
      workspaceRoot: root,
      cwd: root,
      terminalPrivate: false,
      activeFile: null,
    }),
  });
}

beforeEach(() => {
  vi.resetAllMocks();
  usePreferencesStore.setState({ memoryEnabled: false });
  mocks.invoke.mockResolvedValue({
    global: {
      path: "C:/Users/user/.rcode/AGENTS.md",
      content: "GLOBAL_RULE",
      exists: true,
    },
    project: {
      path: "D:/project/AGENTS.md",
      content: "PROJECT_RULE",
      exists: true,
    },
  });
  mocks.business.mockResolvedValue({
    tools: {},
    wrap: (stream: ReadableStream) => stream,
    dispose: vi.fn(),
  });
  mocks.sdk.mockResolvedValue({
    toUIMessageStream: () =>
      new ReadableStream({ start: (controller) => controller.close() }),
  });
  mocks.rust.mockResolvedValue(
    new ReadableStream({ start: (controller) => controller.close() }),
  );
});

describe("project memory at run start", () => {
  it.each(["sdk", "rust"] as const)(
    "loads the index only when enabled on the %s path",
    async (path) => {
      mocks.nativeModel.mockReturnValue(
        path === "rust" ? { model: "model" } : null,
      );
      await transport().sendMessages({ messages: [] });
      expect(mocks.invoke.mock.calls.map(([name]) => name)).not.toContain(
        "agent_memory_prepare",
      );
      const instructions = await mocks.invoke();
      mocks.invoke.mockImplementation(async (name) =>
        name === "agent_memory_prepare"
          ? { id: "scope", index: "- saved fact" }
          : instructions,
      );
      usePreferencesStore.setState({ memoryEnabled: true });
      await transport().sendMessages({ messages: [] });
      expect(mocks.invoke).toHaveBeenCalledWith("agent_memory_prepare", {
        cwd: "D:/project",
      });
      expect(path === "rust" ? mocks.rust : mocks.sdk).toHaveBeenLastCalledWith(
        expect.objectContaining({
          memory: { id: "scope", index: "- saved fact" },
        }),
        expect.anything(),
      );
    },
  );
});

describe("file-backed agent instructions", () => {
  it("prevents every model path when model configuration is empty", async () => {
    const chat = createContextAwareTransport({
      getKeys: () => EMPTY_PROVIDER_KEYS,
      toolContext: context,
      getModelId: () => "gpt-5.6",
      getCustomEndpoints: () => [],
      getAgentPersona: () => null,
      getLive: () => ({
        workspaceRoot: null,
        cwd: null,
        terminalPrivate: false,
        activeFile: null,
      }),
    });
    await expect(chat.sendMessages({ messages: [] })).rejects.toThrow(
      "请先配置模型",
    );
    expect(mocks.invoke).not.toHaveBeenCalled();
    expect(mocks.sdk).not.toHaveBeenCalled();
    expect(mocks.rust).not.toHaveBeenCalled();
  });
  it.each(["sdk", "rust"] as const)(
    "snapshots permissions before async preparation for the %s path",
    async (path) => {
      mocks.nativeModel.mockReturnValue(
        path === "rust" ? { model: "model" } : null,
      );
      let mode: PermissionMode = "edit";
      let plan = true;
      const chat = createContextAwareTransport({
        getKeys: () => EMPTY_PROVIDER_KEYS,
        toolContext: context,
        getModelId: () => "model",
        getAgentPersona: () => null,
        getLive: () => ({
          workspaceRoot: "D:/project",
          cwd: "D:/project",
          terminalPrivate: false,
          activeFile: null,
        }),
        getPermissionMode: () => mode,
        getPlanMode: () => plan,
        prepareWorkspace: async () => {
          mode = "full-access";
          plan = false;
        },
      });
      await chat.sendMessages({ messages: [] });
      expect(mocks[path].mock.calls[0][0]).toEqual(
        expect.objectContaining({ permissionMode: "edit", planMode: true }),
      );
      await chat.sendMessages({ messages: [] });
      expect(mocks[path].mock.calls[1][0]).toEqual(
        expect.objectContaining({
          permissionMode: "full-access",
          planMode: false,
        }),
      );
    },
  );
  it("waits for workspace preparation and prevents every model path on failure", async () => {
    let rejectPreparation: (error: Error) => void = () => {};
    const prepared = new Promise<void>((_, reject) => {
      rejectPreparation = reject;
    });
    const request = transport(null, () => prepared).sendMessages({
      messages: [],
    });
    expect(mocks.invoke).not.toHaveBeenCalled();
    rejectPreparation(new Error("Default directory unavailable"));
    await expect(request).rejects.toThrow("Default directory unavailable");
    expect(mocks.invoke).not.toHaveBeenCalled();
    expect(mocks.sdk).not.toHaveBeenCalled();
    expect(mocks.rust).not.toHaveBeenCalled();
  });
  it.each(["sdk", "rust"] as const)(
    "passes both instruction files to the %s execution path",
    async (path) => {
      mocks.nativeModel.mockReturnValue(
        path === "rust" ? { model: "model" } : null,
      );
      await transport().sendMessages({ messages: [] });
      expect(mocks.invoke).toHaveBeenCalledExactlyOnceWith(
        "agent_instructions_read",
        {
          cwd: "D:/project",
          workspace: { kind: "local" },
        },
      );
      expect(mocks[path]).toHaveBeenCalledWith(
        expect.objectContaining({
          globalInstructions: "GLOBAL_RULE",
          projectInstructions: "PROJECT_RULE",
          agentPersona: { name: "Coder", instructions: "ROLE_RULE" },
        }),
        ...(path === "rust" ? [{ model: "model" }] : [expect.anything()]),
      );
    },
  );

  it("reads disk again next turn so external edits and deletions take effect immediately", async () => {
    const chat = transport();
    await chat.sendMessages({ messages: [] });
    mocks.invoke.mockResolvedValue({
      global: { path: "global", content: "EDITED", exists: true },
      project: { path: "project", content: "", exists: false },
    });
    await chat.sendMessages({ messages: [] });
    expect(mocks.invoke).toHaveBeenCalledTimes(2);
    expect(mocks.sdk).toHaveBeenLastCalledWith(
      expect.objectContaining({
        globalInstructions: "EDITED",
        projectInstructions: "",
      }),
      expect.anything(),
    );
  });

  it("loads global instructions without a selected project", async () => {
    mocks.invoke.mockResolvedValue({
      global: { path: "global", content: "GLOBAL_RULE", exists: true },
      project: null,
    });
    await transport(null).sendMessages({ messages: [] });
    expect(mocks.sdk).toHaveBeenCalledWith(
      expect.objectContaining({
        globalInstructions: "GLOBAL_RULE",
        projectInstructions: null,
      }),
      expect.anything(),
    );
  });

  it("surfaces instruction read errors instead of silently sending a request without rules", async () => {
    mocks.invoke.mockRejectedValue(
      new Error("Instruction file cannot be read"),
    );
    await expect(transport().sendMessages({ messages: [] })).rejects.toThrow(
      "Instruction file cannot be read",
    );
    expect(mocks.sdk).not.toHaveBeenCalled();
    expect(mocks.rust).not.toHaveBeenCalled();
  });
});

describe("reasoning run boundary", () => {
  it.each(["sdk", "rust"] as const)(
    "freezes endpoints, protocol, keys and local configuration before preparing the %s run",
    async (path) => {
      mocks.nativeModel.mockReturnValue(
        path === "rust" ? { model: "model" } : null,
      );
      const endpoint = {
        id: "test",
        name: "Test",
        baseURL: "https://example.com/v1",
        modelId: "model",
        contextLimit: 128000,
        protocol: "responses" as const,
        models: [{ id: "model", reasoningLevels: [] }],
      };
      const endpointKeys = { test: "fixture-key" };
      const keys = { ...EMPTY_PROVIDER_KEYS, openai: "fixture-provider-key" };
      let localId = "initial-local";
      const chat = createContextAwareTransport({
        getKeys: () => keys,
        toolContext: context,
        getModelId: () => "compat-test/model",
        getCustomEndpoints: () => [endpoint],
        getCustomEndpointKeys: () => endpointKeys,
        getLmstudioModelId: () => localId,
        getAgentPersona: () => null,
        getLive: () => ({
          workspaceRoot: null,
          cwd: null,
          terminalPrivate: false,
          activeFile: null,
        }),
        prepareWorkspace: async () => {
          endpoint.baseURL = "https://changed.example/v1";
          endpoint.models[0].id = "deleted";
          endpointKeys.test = "changed-key";
          keys.openai = "changed-provider-key";
          localId = "changed-local";
        },
      });
      await chat.sendMessages({ messages: [] });
      expect(mocks[path].mock.calls[0][0]).toMatchObject({
        customEndpoints: [
          {
            baseURL: "https://example.com/v1",
            protocol: "responses",
            models: [{ id: "model" }],
          },
        ],
        customEndpointKeys: { test: "fixture-key" },
        keys: { openai: "fixture-provider-key" },
        lmstudioModelId: "initial-local",
      });
    },
  );
  it.each(["sdk", "rust"] as const)(
    "freezes model and reasoning before preparing the %s run",
    async (path) => {
      mocks.nativeModel.mockReturnValue(
        path === "rust" ? { model: "model" } : null,
      );
      let modelId = "compat-test/gpt-5.6";
      let level = "low";
      const chat = createContextAwareTransport({
        getKeys: () => EMPTY_PROVIDER_KEYS,
        toolContext: context,
        getModelId: () => modelId,
        getReasoningSelection: () => ({ modelId, level }),
        getCustomEndpoints: () => [
          {
            id: "test",
            name: "Test",
            baseURL: "https://example.com/v1",
            modelId: "gpt-5.6",
            contextLimit: 128000,
            models: [{ id: "gpt-5.6", reasoningLevels: ["low", "high"] }],
          },
        ],
        getAgentPersona: () => null,
        getLive: () => ({
          workspaceRoot: null,
          cwd: null,
          terminalPrivate: false,
          activeFile: null,
        }),
        prepareWorkspace: async () => {
          modelId = "other";
          level = "high";
        },
      });
      await chat.sendMessages({ messages: [] });
      const options = (path === "rust" ? mocks.rust : mocks.sdk).mock
        .calls[0][0];
      expect(options.modelId).toBe("compat-test/gpt-5.6");
      expect(options.reasoningLevel).toBe("low");
    },
  );
  it("rejects stale levels before reading instructions or sending", async () => {
    const chat = createContextAwareTransport({
      getKeys: () => EMPTY_PROVIDER_KEYS,
      toolContext: context,
      getModelId: () => "compat-test/gpt-5.6",
      getReasoningSelection: () => ({
        modelId: "compat-test/gpt-5.6",
        level: "removed",
      }),
      getCustomEndpoints: () => [
        {
          id: "test",
          name: "Test",
          baseURL: "https://example.com/v1",
          modelId: "gpt-5.6",
          contextLimit: 128000,
          models: [{ id: "gpt-5.6", reasoningLevels: ["low", "high"] }],
        },
      ],
      getAgentPersona: () => null,
      getLive: () => ({
        workspaceRoot: null,
        cwd: null,
        terminalPrivate: false,
        activeFile: null,
      }),
    });
    await expect(chat.sendMessages({ messages: [] })).rejects.toThrow(
      "reasoning level",
    );
    expect(mocks.invoke).not.toHaveBeenCalled();
    expect(mocks.sdk).not.toHaveBeenCalled();
    expect(mocks.rust).not.toHaveBeenCalled();
  });
});

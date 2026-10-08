import { beforeEach, describe, expect, it, vi } from "vitest";
import { generateText } from "ai";
import {
  buildConfiguredLanguageModel,
  type RunAgentOptions,
} from "@/modules/ai/lib/agent";
import { resolveNativeModel } from "@/modules/ai/lib/nativeProtocol";
import { runSubagent } from "@/modules/ai/agents/runSubagent";
import {
  configuredSubagents,
  EMPTY_SUBAGENT_CONFIG,
} from "@/modules/ai/agents/config";
import { endpointModelSelectionId } from "@/modules/ai/config";
import { EMPTY_PROVIDER_KEYS } from "@/modules/ai/lib/keyring";
import type { ToolContext } from "@/modules/ai/tools/context";

vi.mock("ai", async (original) => ({
  ...(await original<typeof import("ai")>()),
  generateText: vi.fn(),
}));
vi.mock("@/modules/ai/lib/agent", () => ({
  buildConfiguredLanguageModel: vi.fn(),
}));
vi.mock("@/modules/ai/lib/nativeProtocol", () => ({
  resolveNativeModel: vi.fn(() => null),
}));
vi.mock("@/modules/settings/preferences", () => ({
  usePreferencesStore: { getState: () => ({}) },
}));
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => ({ kind: "wsl" }),
}));
vi.mock("@/modules/ai/store/chatStore", () => ({
  useChatStore: { getState: () => ({}) },
}));
vi.mock("@/modules/ai/tools/fs", () => ({
  buildFsTools: () => ({ read_file: {}, write_file: {}, list_directory: {} }),
}));
vi.mock("@/modules/ai/tools/search", () => ({
  buildSearchTools: () => ({ grep: {}, glob: {} }),
}));

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(generateText).mockResolvedValue({
    text: "verified",
    steps: [],
  } as unknown as Awaited<ReturnType<typeof generateText>>);
});
describe("SDK subagent execution", () => {
  it("uses the child model, reasoning, isolated context and exact tools on the fallback path", async () => {
    const endpoint = {
      id: "test",
      name: "Test",
      baseURL: "https://example.com/v1",
      contextLimit: 64000,
      modelId: "model",
      models: [
        {
          id: "model",
          reasoningLevels: ["low", "high"],
          useRecommendedConfig: false,
          maxOutputTokens: 8192,
        },
      ],
    };
    const modelId = endpointModelSelectionId(endpoint, endpoint.models[0]);
    const def = {
      ...configuredSubagents(EMPTY_SUBAGENT_CONFIG)[0],
      modelId,
      reasoningLevel: "low",
      tools: ["grep"],
      injectAgentsMd: true,
    };
    const context = {
      getSessionId: () => "parent",
      readCache: new Map([["file", "parent-data"]]),
    } as unknown as ToolContext;
    const parent: RunAgentOptions = {
      modelId: "parent-model",
      reasoningLevel: "high",
      keys: EMPTY_PROVIDER_KEYS,
      toolContext: context,
      uiMessages: [
        {
          id: "old",
          role: "user",
          parts: [{ type: "text", text: "parent history" }],
        },
      ],
      customEndpoints: [endpoint],
      globalInstructions: "GLOBAL",
      projectInstructions: "PROJECT",
    };
    const abort = new AbortController();
    const result = await runSubagent({
      type: def.id,
      definition: def,
      parentOptions: parent,
      keys: EMPTY_PROVIDER_KEYS,
      modelId: "parent-model",
      toolContext: context,
      prompt: "investigate",
      abortSignal: abort.signal,
    });
    expect(result.summary).toBe("verified");
    expect(buildConfiguredLanguageModel).toHaveBeenCalledWith(
      modelId,
      EMPTY_PROVIDER_KEYS,
      expect.objectContaining({ reasoningLevel: "low" }),
    );
    const options = vi.mocked(resolveNativeModel).mock.calls[0][0];
    expect(options.toolContext.getSessionId()).not.toBe("parent");
    expect(options.toolContext.readCache.size).toBe(0);
    expect(options.uiMessages).toHaveLength(1);
    expect(options).toMatchObject({
      planMode: true,
      permissionMode: "ask",
      abortSignal: abort.signal,
    });
    expect(generateText).toHaveBeenCalledWith(
      expect.objectContaining({
        tools: { grep: {} },
        prompt: "investigate",
        maxOutputTokens: 8192,
        abortSignal: abort.signal,
        system: expect.stringContaining("GLOBAL\n\nPROJECT"),
      }),
    );
  });
  it("rejects disabled definitions before resolving a model or building tools", async () => {
    const def = {
      ...configuredSubagents(EMPTY_SUBAGENT_CONFIG)[0],
      enabled: false,
    };
    await expect(
      runSubagent({
        type: def.id,
        definition: def,
        keys: EMPTY_PROVIDER_KEYS,
        modelId: "parent",
        toolContext: {} as ToolContext,
        prompt: "test",
      }),
    ).rejects.toThrow("disabled");
    expect(resolveNativeModel).not.toHaveBeenCalled();
  });
});

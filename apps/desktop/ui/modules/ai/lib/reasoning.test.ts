import { describe, expect, it, vi } from "vitest";
import { effectiveCustomModel, type CustomEndpoint } from "@/modules/ai/config";
import {
  commitCustomModelDraft,
  createCustomModelDraft,
  restoreCustomModelDraft,
  toggleCustomModelSmart,
} from "@/modules/ai/lib/customModelDraft";
import { EMPTY_PROVIDER_KEYS } from "@/modules/ai/lib/keyring";
import { resolveNativeModel } from "@/modules/ai/lib/nativeProtocol";
import {
  parseReasoningLevelMap,
  reasoningRequestBody,
  selectedReasoningLevel,
  validateReasoningBody,
  validateReasoningLevels,
  withReasoningBody,
} from "@/modules/ai/lib/reasoning";
import type { ToolContext } from "@/modules/ai/tools/context";

describe("model reasoning configuration", () => {
  it("preserves explicit empty levels across save, manual mode and reload", () => {
    const draft = {
      ...createCustomModelDraft(),
      id: "gpt-5.6",
      reasoningLevels: [],
    };
    const saved = commitCustomModelDraft(draft);
    expect(effectiveCustomModel(saved).reasoningLevels).toEqual([]);
    expect(createCustomModelDraft(saved).reasoningLevels).toEqual([]);
    expect(toggleCustomModelSmart(draft, false).reasoningLevels).toEqual([]);
    expect(
      effectiveCustomModel(
        commitCustomModelDraft(restoreCustomModelDraft(draft)),
      ).reasoningLevels,
    ).toContain("max");
  });
  it("keeps the configured order, selects the highest on a new model and rejects stale selections", () => {
    const levels = ["off", "fast", "deep"];
    expect(validateReasoningLevels(levels)).toEqual(levels);
    expect(selectedReasoningLevel("a", levels)).toBe("deep");
    expect(
      selectedReasoningLevel("a", levels, { modelId: "a", level: "fast" }),
    ).toBe("fast");
    expect(
      selectedReasoningLevel("b", ["enabled"], { modelId: "a", level: "fast" }),
    ).toBe("enabled");
    expect(
      selectedReasoningLevel("a", levels, { modelId: "a", level: "removed" }),
    ).toBeUndefined();
    expect(() => validateReasoningLevels(["high", "high"])).toThrow();
  });
  it("validates bounded mappings and disallows changing the agent payload", () => {
    for (const value of [
      { model: "other" },
      { messages: [] },
      { tools: [] },
      { stream: false },
      { reasoning: { headers: {} } },
      { thinking: { budget_tokens: -1 } },
      { enable_thinking: "yes" },
    ])
      expect(() => validateReasoningBody(value)).toThrow();
    expect(() => parseReasoningLevelMap("not-json")).toThrow("JSON");
    expect(() =>
      parseReasoningLevelMap('{"high":{"model":"other"}}'),
    ).toThrow();
    expect(parseReasoningLevelMap('{"high":{}}')).toEqual({ high: {} });
  });
  it("bounds legacy Messages thinking within the model output limit and uses adaptive thinking where supported", () => {
    expect(
      reasoningRequestBody(
        "messages",
        "high",
        undefined,
        4096,
        "claude-sonnet-4-5",
      ),
    ).toEqual({ thinking: { type: "enabled", budget_tokens: 4095 } });
    expect(
      reasoningRequestBody(
        "messages",
        "max",
        undefined,
        32000,
        "claude-opus-4-8",
      ),
    ).toEqual({
      thinking: { type: "adaptive" },
      output_config: { effort: "max" },
    });
    expect(reasoningRequestBody("messages", "disabled")).toEqual({
      thinking: { type: "disabled" },
    });
    expect(() =>
      reasoningRequestBody("messages", "high", undefined, 512),
    ).toThrow();
  });
  it.each(["chat_completions", "responses", "messages"] as const)(
    "projects the selected level into native %s requests",
    (protocol) => {
      const endpoint: CustomEndpoint = {
        id: "test",
        name: "Test",
        baseURL: "https://example.com/v1",
        modelId: "gpt-5.6",
        contextLimit: 128000,
        protocol,
        models: [{ id: "gpt-5.6", reasoningLevels: ["low", "high"] }],
      };
      const native = resolveNativeModel({
        modelId: "compat-test/gpt-5.6",
        reasoningLevel: "low",
        customEndpoints: [endpoint],
        keys: EMPTY_PROVIDER_KEYS,
        uiMessages: [],
        toolContext: {} as ToolContext,
      });
      expect(native?.reasoningBody).toEqual(
        reasoningRequestBody(protocol, "low"),
      );
      endpoint.models![0].reasoningLevelMap = {
        low: { enable_thinking: true },
      };
      expect(
        resolveNativeModel({
          modelId: "compat-test/gpt-5.6",
          reasoningLevel: "low",
          customEndpoints: [endpoint],
          keys: EMPTY_PROVIDER_KEYS,
          uiMessages: [],
          toolContext: {} as ToolContext,
        })?.reasoningBody,
      ).toEqual({ enable_thinking: true });
    },
  );
  it("passes the same mapping through SDK fetch while preserving transport and messages", async () => {
    const fetcher = vi.fn().mockResolvedValue(new Response("{}"));
    const patched = withReasoningBody(fetcher, {
      reasoning: { effort: "xhigh" },
    });
    const signal = new AbortController().signal;
    await patched("https://example.com", {
      body: JSON.stringify({
        model: "model",
        messages: ["hello"],
        stream: true,
      }),
      signal,
      headers: { "Content-Type": "application/json" },
    });
    const init = fetcher.mock.calls[0][1];
    expect(JSON.parse(init.body)).toEqual({
      model: "model",
      messages: ["hello"],
      stream: true,
      reasoning: { effort: "xhigh" },
    });
    expect(init.signal).toBe(signal);
    expect(withReasoningBody(fetcher)).toBe(fetcher);
  });
});

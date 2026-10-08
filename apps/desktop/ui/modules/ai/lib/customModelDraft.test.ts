import { describe, expect, it } from "vitest";
import {
  compatModelIdForEndpoint,
  effectiveCustomModel,
  endpointModels,
  endpointModelSelectionId,
  resolveEndpointModel,
  type CustomEndpoint,
} from "@/modules/ai/config";
import {
  commitCustomModelDraft,
  createCustomModelDraft,
  restoreCustomModelDraft,
  toggleCustomModelSmart,
} from "@/modules/ai/lib/customModelDraft";
import { resolveNativeModel } from "@/modules/ai/lib/nativeProtocol";
import { EMPTY_PROVIDER_KEYS } from "@/modules/ai/lib/keyring";
import type { ToolContext } from "@/modules/ai/tools/context";

describe("custom model recommendations and overrides", () => {
  it("allows selecting manual mode before the model ID is entered", () => {
    const manual = toggleCustomModelSmart(createCustomModelDraft(), false);
    expect(manual.smart).toBe(false);
    expect(manual.context).toBe("128000");
    expect(() => commitCustomModelDraft(manual)).toThrow("model ID");
  });
  it("inherits recommendations without persisting an effective snapshot", () => {
    const model = commitCustomModelDraft({
      ...createCustomModelDraft(),
      id: "gpt-5.6",
    });
    expect(model.contextLimit).toBeUndefined();
    expect(effectiveCustomModel(model)).toMatchObject({
      known: true,
      contextLimit: 1_050_000,
      maxOutputTokens: 32_000,
      vision: true,
    });
  });
  it.each([
    "deepseek-v4.1-flash",
    "deepseek-flash",
    "deepseek/deepseek-v4.1-flash",
  ])(
    "uses the 1M DeepSeek Flash recommendation for %s through native execution",
    (id) => {
      const model = commitCustomModelDraft({ ...createCustomModelDraft(), id });
      expect(model.contextLimit).toBeUndefined();
      expect(effectiveCustomModel(model)).toMatchObject({
        known: true,
        contextLimit: 1_000_000,
        maxOutputTokens: 32_000,
        vision: true,
        reasoning: true,
      });
      const endpoint: CustomEndpoint = {
        id: "deepseek-preview",
        name: "DeepSeek",
        baseURL: "https://api.example.com/v1",
        modelId: id,
        contextLimit: 128_000,
        protocol: "chat_completions",
        models: [model],
      };
      expect(
        resolveNativeModel({
          modelId: endpointModelSelectionId(endpoint, model),
          customEndpoints: [endpoint],
          keys: EMPTY_PROVIDER_KEYS,
          uiMessages: [],
          toolContext: {} as ToolContext,
        }),
      ).toMatchObject({
        model: id,
        contextLimit: 1_000_000,
        maxOutputTokens: 32_000,
        imageInput: true,
      });
    },
  );
  it("keeps explicit DeepSeek limits and image input settings over updated recommendations", () => {
    const model = commitCustomModelDraft({
      ...createCustomModelDraft(),
      id: "deepseek-v4.1-flash",
      context: "128000",
      output: "8192",
      vision: false,
    });
    expect(effectiveCustomModel(model)).toMatchObject({
      contextLimit: 128_000,
      maxOutputTokens: 8192,
      vision: false,
    });
    const reset = commitCustomModelDraft(
      restoreCustomModelDraft(createCustomModelDraft(model)),
    );
    expect(effectiveCustomModel(reset)).toMatchObject({
      contextLimit: 1_000_000,
      maxOutputTokens: 32_000,
      vision: true,
    });
  });
  it("preserves explicit overrides, including false, while the model ID changes", () => {
    const draft = {
      ...createCustomModelDraft(),
      id: "gpt-5.6",
      context: "65536",
      vision: false,
    };
    expect(
      effectiveCustomModel(commitCustomModelDraft({ ...draft, id: "unknown" })),
    ).toMatchObject({
      contextLimit: 65536,
      maxOutputTokens: 16384,
      vision: false,
    });
    const restored = commitCustomModelDraft(restoreCustomModelDraft(draft));
    expect(restored.contextLimit).toBeUndefined();
    expect(restored.vision).toBeUndefined();
    expect(effectiveCustomModel(restored).vision).toBe(true);
  });
  it("freezes manual settings and clears them only when recommended mode is restored", () => {
    const manual = commitCustomModelDraft(
      toggleCustomModelSmart(
        { ...createCustomModelDraft(), id: "gpt-5.6" },
        false,
      ),
    );
    expect(manual).toMatchObject({
      useRecommendedConfig: false,
      contextLimit: 1_050_000,
      maxOutputTokens: 32_000,
      vision: true,
    });
    expect(
      toggleCustomModelSmart(createCustomModelDraft(manual), true).context,
    ).toBe("");
  });
  it.each(["1000", "4000001", "1e5", "12.5", "-1"])(
    "rejects context %s before persistence",
    (context) => {
      expect(() =>
        commitCustomModelDraft({
          ...createCustomModelDraft(),
          id: "unknown",
          context,
        }),
      ).toThrow();
    },
  );
  it.each([true, false])(
    "does not swallow an invalid draft when toggling smart mode to %s",
    (smart) => {
      expect(() =>
        toggleCustomModelSmart(
          { ...createCustomModelDraft(), id: "gpt-5.6", context: "invalid" },
          smart,
        ),
      ).toThrow();
      expect(() =>
        commitCustomModelDraft({
          ...createCustomModelDraft(),
          id: "unknown",
          context: "8192",
          output: "8193",
        }),
      ).toThrow("Maximum output");
    },
  );
});

describe("provider model identity and native execution", () => {
  const endpoint: CustomEndpoint = {
    id: "custom",
    name: "Gateway",
    baseURL: "http://127.0.0.1:1234/v1",
    modelId: "vendor/model:a",
    contextLimit: 128_000,
    protocol: "messages",
    models: [
      { id: "vendor/model:a", useRecommendedConfig: true },
      {
        id: "second",
        contextLimit: 65536,
        maxOutputTokens: 4096,
        vision: false,
      },
    ],
  };
  it("keeps multiple model identifiers distinct under a shared endpoint and key", () => {
    const models = endpointModels(endpoint);
    const first = endpointModelSelectionId(endpoint, models[0]);
    const second = endpointModelSelectionId(endpoint, models[1]);
    expect(first).not.toBe(second);
    expect(resolveEndpointModel(first, [endpoint])?.model.id).toBe(
      "vendor/model:a",
    );
    expect(resolveEndpointModel(second, [endpoint])?.model.id).toBe("second");
    const config = resolveNativeModel({
      modelId: second,
      customEndpoints: [endpoint],
      keys: EMPTY_PROVIDER_KEYS,
      uiMessages: [],
      toolContext: {} as ToolContext,
    });
    expect(config).toMatchObject({
      model: "second",
      protocol: "messages",
      contextLimit: 65536,
      maxOutputTokens: 4096,
      imageInput: false,
      secretAccount: "compat-custom-api-key",
    });
  });
  it("never falls back to another model when an explicit model has been removed", () => {
    const stale = compatModelIdForEndpoint(endpoint.id, "removed");
    expect(resolveEndpointModel(stale, [endpoint])).toBeUndefined();
    expect(
      resolveEndpointModel("compat-custom/%ZZ", [endpoint]),
    ).toBeUndefined();
    expect(() =>
      resolveNativeModel({
        modelId: stale,
        customEndpoints: [endpoint],
        keys: EMPTY_PROVIDER_KEYS,
        uiMessages: [],
        toolContext: {} as ToolContext,
      }),
    ).toThrow();
  });
  it("preserves legacy manual limits and treats an explicit empty model list as empty", () => {
    const legacy = {
      ...endpoint,
      models: undefined,
      modelId: "legacy",
      contextLimit: 64000,
    };
    expect(effectiveCustomModel(endpointModels(legacy)[0])).toMatchObject({
      contextLimit: 64000,
      vision: true,
    });
    expect(endpointModelSelectionId(legacy, endpointModels(legacy)[0])).toBe(
      "compat-custom",
    );
    expect(endpointModels({ ...legacy, models: [] })).toEqual([]);
  });
});

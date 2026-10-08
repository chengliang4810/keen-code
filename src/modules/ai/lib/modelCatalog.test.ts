import { afterEach, describe, expect, it } from "vitest";
import {
  createModelCatalogIndex,
  installModelCatalog,
  resolveCatalogModel,
  type ModelCatalogSnapshot,
} from "@/modules/ai/lib/modelCatalog";
import {
  effectiveCustomModel,
  endpointModelSelectionId,
  recommendedModelConfig,
  type CustomEndpoint,
} from "@/modules/ai/config";
import {
  commitCustomModelDraft,
  createCustomModelDraft,
  toggleCustomModelSmart,
} from "@/modules/ai/lib/customModelDraft";
import { resolveNativeModel } from "@/modules/ai/lib/nativeProtocol";
import { EMPTY_PROVIDER_KEYS } from "@/modules/ai/lib/keyring";
import type { ToolContext } from "@/modules/ai/tools/context";

const canonicalId = "deepseek/deepseek-v4.1-flash";
const snapshot: ModelCatalogSnapshot = {
  schemaVersion: 1,
  fetchedAt: 1,
  providers: [
    {
      id: "deepseek",
      api: "https://api.deepseek.com",
      models: [
        {
          id: "deepseek-flash",
          canonicalId,
          contextLimit: 1_000_000,
          outputLimit: 393_216,
          vision: true,
          reasoning: true,
          reasoningLevels: ["none", "low", "high", "max"],
        },
      ],
    },
    {
      id: "openrouter",
      api: "https://openrouter.ai/api/v1",
      models: [
        {
          id: canonicalId,
          canonicalId,
          contextLimit: 1_048_576,
          outputLimit: 943_718,
          vision: true,
          reasoning: true,
        },
      ],
    },
    {
      id: "openai",
      models: [
        {
          id: "gpt-5",
          canonicalId: "openai/gpt-5",
          contextLimit: 400_000,
          outputLimit: 128_000,
          vision: false,
          reasoning: false,
        },
      ],
    },
  ],
};

afterEach(() => installModelCatalog(null));

describe("local models.dev recommendations", () => {
  it("matches provider URLs before canonical identity without rewriting the model ID", () => {
    const index = createModelCatalogIndex(snapshot);
    expect(
      resolveCatalogModel(index, canonicalId, "https://openrouter.ai/api/v1/")
        ?.contextLimit,
    ).toBe(1_048_576);
    expect(
      resolveCatalogModel(
        index,
        "deepseek-v4.1-flash",
        "https://api.deepseek.com/v1",
      )?.contextLimit,
    ).toBe(1_000_000);
    expect(
      resolveCatalogModel(index, canonicalId, "https://proxy.example/v1")?.id,
    ).toBe("deepseek-flash");
    expect(
      resolveCatalogModel(index, "gpt-5", "https://api.openai.com/v1")
        ?.contextLimit,
    ).toBe(400_000);
    expect(
      resolveCatalogModel(
        index,
        "deepseek-flash",
        "https://openrouter.ai.evil.example/api/v1",
      )?.contextLimit,
    ).toBe(1_000_000);
  });

  it("does not borrow another provider's limits when a recognized endpoint lacks that model", () => {
    expect(
      resolveCatalogModel(
        createModelCatalogIndex(snapshot),
        "gpt-5",
        "https://api.deepseek.com",
      ),
    ).toBeUndefined();
  });

  it("leaves conflicting unowned aliases unresolved", () => {
    const index = createModelCatalogIndex({
      ...snapshot,
      providers: [
        { id: "a", models: [{ id: "unknown", contextLimit: 32_000 }] },
        { id: "b", models: [{ id: "unknown", contextLimit: 64_000 }] },
      ],
    });
    expect(resolveCatalogModel(index, "unknown")).toBeUndefined();
    expect(resolveCatalogModel(index, "missing")).toBeUndefined();
  });

  it("uses catalog false values, output limits and reasoning choices over bundled guesses", () => {
    installModelCatalog(snapshot);
    expect(recommendedModelConfig("gpt-5")).toMatchObject({
      source: "models.dev",
      contextLimit: 400_000,
      vision: false,
      reasoning: false,
      reasoningLevels: [],
    });
    expect(recommendedModelConfig("deepseek-v4.1-flash")).toMatchObject({
      source: "models.dev",
      reasoningLevels: ["none", "low", "high", "max"],
    });
    installModelCatalog({
      ...snapshot,
      providers: [
        {
          id: "test",
          models: [
            { id: "small-output", contextLimit: 128_000, outputLimit: 4096 },
          ],
        },
      ],
    });
    expect(recommendedModelConfig("small-output").maxOutputTokens).toBe(4096);
  });

  it("keeps manual fields and explicit empty reasoning choices across full catalog updates", () => {
    installModelCatalog(snapshot);
    const model = commitCustomModelDraft({
      ...createCustomModelDraft(),
      id: canonicalId,
      context: "65536",
      output: "8192",
      vision: false,
      reasoningLevels: [],
    });
    const frozen = commitCustomModelDraft(
      toggleCustomModelSmart(
        { ...createCustomModelDraft(), id: canonicalId },
        false,
        "https://openrouter.ai/api/v1",
      ),
    );
    installModelCatalog({
      ...snapshot,
      providers: snapshot.providers.map((provider) => ({
        ...provider,
        models: provider.models.map((model) => ({
          ...model,
          contextLimit: 2_000_000,
        })),
      })),
    });
    expect(effectiveCustomModel(model)).toMatchObject({
      contextLimit: 65536,
      maxOutputTokens: 8192,
      vision: false,
      reasoningLevels: [],
    });
    expect(effectiveCustomModel(frozen).contextLimit).toBe(1_048_576);
    expect(effectiveCustomModel({ id: canonicalId }).contextLimit).toBe(
      2_000_000,
    );
  });

  it("sends the same provider-specific values through native execution as the form", () => {
    installModelCatalog(snapshot);
    const model = commitCustomModelDraft(
      { ...createCustomModelDraft(), id: canonicalId },
      true,
      "https://openrouter.ai/api/v1",
    );
    const endpoint: CustomEndpoint = {
      id: "test",
      name: "Test",
      baseURL: "https://openrouter.ai/api/v1",
      modelId: canonicalId,
      contextLimit: 128_000,
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
      model: canonicalId,
      contextLimit: 1_048_576,
      imageInput: true,
      maxOutputTokens: 32_000,
    });
  });

  it("retains the last valid index when replacement data is invalid", () => {
    installModelCatalog(snapshot);
    expect(() =>
      installModelCatalog({ ...snapshot, schemaVersion: 2 }),
    ).toThrow();
    expect(recommendedModelConfig("deepseek-v4.1-flash").source).toBe(
      "models.dev",
    );
    expect(() =>
      createModelCatalogIndex({
        ...snapshot,
        providers: [{ id: "bad", models: [{ id: "m", contextLimit: -1 }] }],
      }),
    ).toThrow();
  });
});

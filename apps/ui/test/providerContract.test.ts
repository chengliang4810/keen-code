import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "vitest";
import {
  completeModelConfigDataSchema,
  modelConfigDataSchema,
} from "../../../packages/shared/src/model-config.ts";
import { providerConfigDataSchema } from "../../../packages/provider/src/config/provider-data-schema.ts";
import { completeNewModelSelection } from "../../../packages/provider/src/model-selection-config.ts";
import { validateModelSelectionOptions } from "../../../packages/provider/src/registry.ts";
import { resolveModelThoughtOption } from "../../../packages/ui/src/lib/modelThoughtOption.ts";
import {
  createProviderModelDraftValues,
  resolveProviderModelDraftCommit,
} from "../../../packages/ui/src/settings/model-provider-section/ProviderModelMetadata.ts";

// 此夹具由 Rust façade 的实际序列化生成；公开设置和事件必须遵守源项目 schema，
// 且不能把内部 provider 凭据随视图发布到 renderer。
const fixture = JSON.parse(readFileSync(resolve(import.meta.dirname,
  "../../../tooling/native-live/workflow-contract-fixtures/provider_facade_contract.json"), "utf8"));

test("Rust provider settings and model selection preserve source config contracts", () => {
  for (const view of [fixture.settingsGetView, fixture.settingsEvent]) {
    expect(Number.isSafeInteger(view.revision)).toBe(true);
    expect(view.providerOrder).toEqual(view.providers.map((provider: any) => provider.providerId));
    for (const provider of view.providers) {
      providerConfigDataSchema.parse(provider.effectiveConfig);
      if (provider.personalConfig) providerConfigDataSchema.parse(provider.personalConfig);
      for (const model of provider.models) {
        completeModelConfigDataSchema.parse(model.effectiveConfig);
        if (model.effectiveBuiltinConfig) completeModelConfigDataSchema.parse(model.effectiveBuiltinConfig);
        if (!model.enabled) {
          expect(model.selectable).toBe(false);
          expect(model.executable).toBe(false);
        }
      }
    }
  }
  expect(fixture.settingsEvent).toEqual(fixture.settingsGetView);
  expect(Number.isSafeInteger(fixture.modelSelectionGetView.revision)).toBe(true);
  for (const provider of fixture.modelSelectionGetView.providers) {
    providerConfigDataSchema.parse(provider.config);
    for (const model of provider.models) {
      completeModelConfigDataSchema.parse(model.config);
      expect(model.config.enabled).toBe(true);
    }
    expect(provider.models.some((model: any) => model.modelId === "disabled-model")).toBe(false);
  }
});

test("Rust provider public views never serialize credential fields", () => {
  const inspect = (value: unknown): void => {
    if (value === null || typeof value !== "object") return;
    for (const [key, child] of Object.entries(value)) {
      expect(key.toLowerCase()).not.toMatch(/^(apikey|api_key|authorization|credential|secret)$/);
      inspect(child);
    }
  };
  inspect(fixture);
});

test("模型没有 reasoningLevel 时仍可执行且不显示档位控件", () => {
  const model = { modelId: "model-without-reasoning", config: { optionSpecs: {} } };
  const view = {
    providers: [{ providerId: "provider-contract", models: [model] }],
  };
  const selection = { providerId: "provider-contract", modelId: model.modelId };

  expect(validateModelSelectionOptions(model, selection)).toEqual({ ok: true });
  expect(completeNewModelSelection(view, selection)).toEqual(selection);
  expect(
    resolveModelThoughtOption({
      modelSelectionView: view as never,
      providerId: selection.providerId,
      modelId: selection.modelId,
    }),
  ).toBeNull();
});

test("完整模型 schema 仍拒绝空 reasoningLevel.values", () => {
  expect(() =>
    completeModelConfigDataSchema.parse({
      enabled: true,
      properties: {
        requiresMfjsToolSchema: false,
        contextWindow: 4096,
        inputFormat: {
          supportsText: true,
          supportsImage: false,
          supportsVideo: false,
          supportsAudio: false,
          supportsPdf: false,
        },
        outputFormat: { supportsText: true },
        supportsToolCall: true,
        supportsJsonSchemaOutput: false,
        supportsNativeWebSearch: false,
        supportsMidConversationSystem: true,
      },
      optionSpecs: {
        reasoningLevel: {
          values: [],
          map: '{"reasoning": reasoningLevel}',
        },
        maxOutputTokens: {
          max: 4096,
          map: '{"max_output_tokens": maxOutputTokens}',
        },
      },
    }),
  ).toThrow();
});

test("个人模型覆盖可以显式保存空 reasoningLevel 并保持目录档位关闭", () => {
  const inheritedConfig = {
    enabled: true,
    properties: {
      requiresMfjsToolSchema: false,
      contextWindow: 131072,
      inputFormat: {
        supportsText: true,
        supportsImage: false,
        supportsVideo: false,
        supportsAudio: false,
        supportsPdf: false,
      },
      outputFormat: { supportsText: true },
      supportsToolCall: true,
      supportsJsonSchemaOutput: true,
      supportsNativeWebSearch: false,
      supportsMidConversationSystem: true,
    },
    optionSpecs: {
      reasoningLevel: {
        values: ["low", "high"],
        map: '{"reasoning_effort": reasoningLevel}',
      },
      maxOutputTokens: { max: 4096, map: '{"max_output_tokens": maxOutputTokens}' },
    },
  } as const;
  const model = {
    kind: "candidate",
    modelId: "model-with-reasoning",
    builtin: false,
    inheritedConfig,
    personalConfig: {},
    useRecommendedConfig: true,
    config: inheritedConfig,
    hasPersonalConfig: false,
    executable: true,
    selectable: true,
  } as never;
  const draft = createProviderModelDraftValues(model);
  draft.reasoningLevelValuesValue = [];
  draft.overriddenFieldsValue = ["reasoningLevelValuesValue"];

  const result = resolveProviderModelDraftCommit({ currentModel: model, draft });

  expect(result.status).toBe("commit");
  if (result.status !== "commit") return;
  expect(result.model.personalConfig.optionSpecs?.reasoningLevel?.values).toEqual([]);
  expect(result.model.config.optionSpecs?.reasoningLevel).toBeUndefined();
  expect(() => modelConfigDataSchema.parse(result.model.personalConfig)).not.toThrow();
});

test("无 reasoningLevel 的模型仍可通过设置编辑器保存其他字段", () => {
  const inheritedConfig = {
    enabled: true,
    properties: {
      requiresMfjsToolSchema: false,
      contextWindow: 131072,
      inputFormat: {
        supportsText: true,
        supportsImage: false,
        supportsVideo: false,
        supportsAudio: false,
        supportsPdf: false,
      },
      outputFormat: { supportsText: true },
      supportsToolCall: true,
      supportsJsonSchemaOutput: true,
      supportsNativeWebSearch: false,
      supportsMidConversationSystem: true,
    },
    optionSpecs: {
      maxOutputTokens: {
        max: 4096,
        map: '{"max_output_tokens": maxOutputTokens}',
      },
    },
  } as const;
  const model = {
    kind: "candidate",
    modelId: "model-without-reasoning",
    builtin: false,
    inheritedConfig,
    personalConfig: {},
    useRecommendedConfig: true,
    config: inheritedConfig,
    hasPersonalConfig: false,
    executable: true,
    selectable: true,
  } as never;
  const draft = createProviderModelDraftValues(model);
  draft.contextWindowValue = "65536";

  const result = resolveProviderModelDraftCommit({ currentModel: model, draft });

  expect(result.status).toBe("commit");
  if (result.status !== "commit") return;
  expect(result.model.personalConfig.properties?.contextWindow).toBe(65536);
  expect(result.model.personalConfig.optionSpecs?.reasoningLevel).toBeUndefined();
  expect(result.model.config.optionSpecs?.reasoningLevel).toBeUndefined();
});

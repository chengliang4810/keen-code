import { describe, expect, it } from "vitest";
import {
  applyModelMetadata,
  DEFAULT_EFFORT,
  effortDisplayLabel,
  effortsForModel,
  hasConfiguredProviderModel,
  isValidEffort,
  pickDefaultEffort,
  reasoningEffortsFromMetadata,
  type ModelOption,
} from "./modelCatalog";
import type { ModelMetadata } from "./api";

describe("hasConfiguredProviderModel", () => {
  const catalog: ModelOption[] = [
    { providerId: "openai", id: "gpt-5", label: "GPT-5" },
  ];

  it("rejects sending when no provider or model is configured", () => {
    expect(hasConfiguredProviderModel(null, null, [])).toBe(false);
    expect(hasConfiguredProviderModel("openai", null, catalog)).toBe(false);
    expect(hasConfiguredProviderModel(null, "gpt-5", catalog)).toBe(false);
  });

  it("accepts only a model belonging to the active provider", () => {
    expect(hasConfiguredProviderModel("openai", "gpt-5", catalog)).toBe(true);
    expect(hasConfiguredProviderModel("anthropic", "gpt-5", catalog)).toBe(
      false,
    );
  });
});

const modelWithEfforts: ModelOption = {
  id: "grok-4.5",
  label: "Grok 4.5",
  reasoningEfforts: [
    {
      id: "high",
      value: "high",
      label: "High Effort",
      description: "Deep",
      isDefault: true,
    },
    {
      id: "medium",
      value: "medium",
      label: "Medium Effort",
      isDefault: false,
    },
    {
      id: "low",
      value: "low",
      label: "Low Effort",
      isDefault: false,
    },
  ],
};

const modelCustomOnly: ModelOption = {
  id: "custom-model",
  label: "Custom",
  reasoningEfforts: [
    { id: "max", value: "max", label: "Max", isDefault: true },
    { id: "min", value: "min", label: "Min" },
  ],
};

describe("effortsForModel", () => {
  it("未查询到推理信息时不猜测静态强度", () => {
    expect(effortsForModel({ id: "x", label: "X" })).toEqual([]);
    expect(effortsForModel(null)).toEqual([]);
    expect(effortsForModel(undefined)).toEqual([]);
  });

  it("returns model efforts when non-empty", () => {
    const list = effortsForModel(modelWithEfforts);
    expect(list).toHaveLength(3);
    expect(list[0].id).toBe("high");
    expect(list[0].label).toBe("High Effort");
  });

  it("prefers explicit catalogEfforts arg over model", () => {
    const override = [{ id: "only" }];
    expect(effortsForModel(modelWithEfforts, override)).toEqual(override);
  });
});

describe("isValidEffort", () => {
  it("没有模型元数据时不接受任何推理强度", () => {
    expect(isValidEffort("low")).toBe(false);
    expect(isValidEffort("medium")).toBe(false);
    expect(isValidEffort("high")).toBe(false);
    expect(isValidEffort("max")).toBe(false);
    expect(isValidEffort("")).toBe(false);
  });

  it("accepts efforts for the selected model when known", () => {
    expect(isValidEffort("high", modelWithEfforts)).toBe(true);
    expect(isValidEffort("max", modelCustomOnly)).toBe(true);
    expect(isValidEffort("min", modelCustomOnly)).toBe(true);
    expect(isValidEffort("medium", modelCustomOnly)).toBe(false);
  });

  it("accepts an efforts array directly", () => {
    expect(isValidEffort("max", modelCustomOnly.reasoningEfforts)).toBe(true);
    expect(isValidEffort("high", modelCustomOnly.reasoningEfforts)).toBe(
      false,
    );
  });
});

describe("pickDefaultEffort", () => {
  it("uses model default flag when present", () => {
    expect(pickDefaultEffort(modelWithEfforts)).toBe("high");
    expect(pickDefaultEffort(modelCustomOnly)).toBe("max");
  });

  it("falls back to medium static default", () => {
    expect(pickDefaultEffort(null)).toBe(DEFAULT_EFFORT);
    expect(pickDefaultEffort({ id: "x", label: "X" })).toBe("medium");
  });
});

describe("effortDisplayLabel", () => {
  it("prefers i18n for known ids over English catalog labels", () => {
    expect(
      effortDisplayLabel(
        { id: "high", label: "High Effort" },
        { high: "高" },
      ),
    ).toBe("高");
    expect(
      effortDisplayLabel(
        { id: "medium", label: "Medium Effort" },
        { medium: "中" },
      ),
    ).toBe("中");
    expect(
      effortDisplayLabel(
        { id: "low", label: "Low Effort" },
        { high: "High", medium: "Medium", low: "Low" },
      ),
    ).toBe("Low");
  });

  it("uses i18n for known ids without catalog label", () => {
    expect(
      effortDisplayLabel("high", {
        high: "High",
        medium: "Medium",
        low: "Low",
      }),
    ).toBe("High");
    expect(effortDisplayLabel({ id: "medium" }, { medium: "中" })).toBe(
      "中",
    );
    expect(effortDisplayLabel("none", { none: "关闭" })).toBe("关闭");
    expect(effortDisplayLabel("xhigh", { xhigh: "极高" })).toBe("极高");
    expect(effortDisplayLabel("max", { max: "最大" })).toBe("最大");
  });

  it("strips shared Effort suffix on non-standard catalog labels", () => {
    expect(
      effortDisplayLabel({ id: "max", label: "Max Effort" }),
    ).toBe("Max");
  });

  it("falls back to raw id", () => {
    expect(effortDisplayLabel("max")).toBe("max");
  });
});

const metadata: ModelMetadata = {
  modelId: "grok-4.5",
  price: {
    inputPerMillion: 2,
    outputPerMillion: 10,
    cacheReadPerMillion: null,
    cacheWritePerMillion: null,
  },
  contextWindow: 500_000,
  maxOutputTokens: 64_000,
  reasoning: {
    supported: true,
    controls: [
      { type: "toggle" },
      { type: "effort", values: ["low", "medium", "high", "xhigh"] },
    ],
    defaultEffort: "medium",
    mandatory: false,
  },
  supportsVision: true,
  sources: {
    price: { catalog: "vercel", matchedModelId: "x-ai/grok-4.5" },
    contextWindow: { catalog: "vercel", matchedModelId: "x-ai/grok-4.5" },
    maxOutputTokens: { catalog: "vercel", matchedModelId: "x-ai/grok-4.5" },
    reasoning: { catalog: "vercel", matchedModelId: "x-ai/grok-4.5" },
    supportsVision: { catalog: "vercel", matchedModelId: "x-ai/grok-4.5" },
  },
  updatedAt: 1,
};

describe("reasoningEffortsFromMetadata", () => {
  it("只投影目录明确给出的 effort 强度并保留默认值", () => {
    expect(reasoningEffortsFromMetadata(metadata.reasoning)).toEqual([
      { id: "low", value: "low", isDefault: false },
      { id: "medium", value: "medium", isDefault: true },
      { id: "high", value: "high", isDefault: false },
      { id: "xhigh", value: "xhigh", isDefault: false },
    ]);
  });

  it("不支持推理或只有开关时不伪造强度", () => {
    expect(
      reasoningEffortsFromMetadata({
        supported: false,
        controls: [],
        defaultEffort: null,
        mandatory: null,
      }),
    ).toEqual([]);
    expect(
      reasoningEffortsFromMetadata({
        supported: true,
        controls: [{ type: "toggle" }],
        defaultEffort: null,
        mandatory: null,
      }),
    ).toEqual([]);
  });
});

describe("applyModelMetadata", () => {
  it("按相同 modelId 附加上下文和动态推理信息", () => {
    const model = applyModelMetadata(
      { id: "grok-4.5", label: "Grok 4.5" },
      metadata,
    );
    expect(model.contextWindow).toBe(500_000);
    expect(model.maxOutputTokens).toBe(64_000);
    expect(model.reasoningSupported).toBe(true);
    expect(model.reasoningEfforts?.map((effort) => effort.id)).toEqual([
      "low",
      "medium",
      "high",
      "xhigh",
    ]);
  });

  it("不同 modelId 不应互相污染", () => {
    const model = { id: "other", label: "Other" };
    expect(applyModelMetadata(model, metadata)).toBe(model);
  });
});

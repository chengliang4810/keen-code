import { describe, expect, it } from "vitest";
import {
  applyModelMetadata,
  DEFAULT_EFFORT,
  effortDisplayLabel,
  effortsForModel,
  findActiveModel,
  formatSessionModelReference,
  hasConfiguredProviderModel,
  isBoundSessionModelReference,
  isValidEffort,
  modelOptionsFromSessionConfig,
  modelIdFromSessionReference,
  pickDefaultEffort,
  providerIdFromSessionReference,
  reasoningEffortsFromMetadata,
  type ModelOption,
} from "./modelCatalog";
import type { ModelMetadata } from "./api";

it("从 Session 路由引用提取目录模型 ID", () => {
  expect(modelIdFromSessionReference("fix-local::hy4-preview")).toBe("hy4-preview");
  expect(modelIdFromSessionReference("vendor::org/model:latest")).toBe("org/model:latest");
  expect(modelIdFromSessionReference("hy3")).toBe("hy3");
});

it("Session 路由引用可往返还原供应商与模型", () => {
  expect(providerIdFromSessionReference("fix-local::hy4-preview")).toBe("fix-local");
  expect(providerIdFromSessionReference("hy3")).toBeNull();
  expect(formatSessionModelReference("fix-local", "hy4-preview")).toBe(
    "fix-local::hy4-preview",
  );
  expect(formatSessionModelReference(null, "hy3")).toBe("hy3");
  expect(formatSessionModelReference("  ", "hy3")).toBe("hy3");
});

it("Host 的 unconfigured 占位值按未选择处理", () => {
  expect(isBoundSessionModelReference("unconfigured")).toBe(false);
  expect(isBoundSessionModelReference("")).toBe(false);
  expect(isBoundSessionModelReference("   ")).toBe(false);
  expect(isBoundSessionModelReference("fix-local::hy3")).toBe(true);
  expect(isBoundSessionModelReference("hy3")).toBe(true);
});

it("标准 ACP Session 配置目录可投影为 Web Composer 模型和推理档位", () => {
  const projection = modelOptionsFromSessionConfig([
    {
      id: "model",
      currentValue: "local::gpt-5",
      options: [
        { value: "local::gpt-5", name: "本机 / gpt-5" },
        { value: "cloud::gpt-5", name: "Cloud / gpt-5" },
        { value: "unconfigured", name: "未配置模型" },
      ],
    },
    {
      id: "reasoning_effort",
      currentValue: "medium",
      options: [
        { value: "low", name: "Low" },
        { value: "medium", name: "Medium" },
      ],
    },
  ]);

  expect(projection.currentModel).toBe("local::gpt-5");
  expect(projection.models).toMatchObject([
    {
      providerId: "local",
      providerLabel: "本机",
      id: "gpt-5",
      label: "gpt-5",
      reasoningSupported: true,
    },
    { providerId: "cloud", providerLabel: "Cloud", id: "gpt-5" },
  ]);
  expect(projection.efforts).toMatchObject([
    { id: "low", isDefault: false },
    { id: "medium", isDefault: true },
  ]);
});

describe("findActiveModel", () => {
  /** 同一模型 ID 出现在两个供应商下，模拟多家网关提供同名模型。 */
  const catalog: ModelOption[] = [
    {
      providerId: "workbuddy",
      providerLabel: "Workbuddy",
      id: "deepseek-v4.1-flash",
      label: "deepseek-v4.1-flash",
    },
    {
      providerId: "workbuddy-ai",
      providerLabel: "WorkBuddyAI",
      id: "deepseek-v4.1-flash",
      label: "deepseek-v4.1-flash",
    },
  ];

  it("按会话供应商精确匹配同名模型", () => {
    expect(findActiveModel("deepseek-v4.1-flash", "workbuddy-ai", catalog)?.providerLabel)
      .toBe("WorkBuddyAI");
    expect(findActiveModel("deepseek-v4.1-flash", "workbuddy", catalog)?.providerLabel)
      .toBe("Workbuddy");
  });

  it("供应商未知时按模型 ID 兜底，供应商已移除模型时不借用同名条目", () => {
    expect(findActiveModel("deepseek-v4.1-flash", null, catalog)?.providerId).toBe(
      "workbuddy",
    );
    expect(findActiveModel("deepseek-v4.1-flash", "other", catalog)).toBeUndefined();
    expect(findActiveModel("", "workbuddy", catalog)).toBeUndefined();
  });
});

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

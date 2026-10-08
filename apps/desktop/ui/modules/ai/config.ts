// Modified for RCode. See NOTICE.
import {
  recommendedReasoningLevels,
  validateReasoningLevels,
  type ReasoningLevelMap,
} from "@/modules/ai/lib/reasoning";
import { catalogModelRecommendation } from "@/modules/ai/lib/modelCatalog";

export const KEYRING_SERVICE = "rcode-ai";

export type ProviderId =
  | "openai"
  | "anthropic"
  | "xai"
  | "cerebras"
  | "groq"
  | "deepseek"
  | "mistral"
  | "openrouter"
  | "openai-compatible"
  | "lmstudio"
  | "mlx"
  | "ollama";

export type ProviderInfo = {
  id: ProviderId;
  label: string;
  keyringAccount: string;
  keyPrefix: string | null;
  consoleUrl: string;
  /** Provider accepts (but does not require) an API key. */
  keyOptional?: boolean;
};

export const PROVIDERS: readonly ProviderInfo[] = [
  {
    id: "openai",
    label: "OpenAI",
    keyringAccount: "openai-api-key",
    keyPrefix: "sk-",
    consoleUrl: "https://platform.openai.com/api-keys",
  },
  {
    id: "anthropic",
    label: "Anthropic",
    keyringAccount: "anthropic-api-key",
    keyPrefix: "sk-ant-",
    consoleUrl: "https://console.anthropic.com/settings/keys",
  },
  {
    id: "xai",
    label: "xAI",
    keyringAccount: "xai-api-key",
    keyPrefix: "xai-",
    consoleUrl: "https://console.x.ai/",
  },
  {
    id: "cerebras",
    label: "Cerebras",
    keyringAccount: "cerebras-api-key",
    keyPrefix: "csk-",
    consoleUrl: "https://cloud.cerebras.ai/",
  },
  {
    id: "groq",
    label: "Groq",
    keyringAccount: "groq-api-key",
    keyPrefix: "gsk_",
    consoleUrl: "https://console.groq.com/keys",
  },
  {
    id: "deepseek",
    label: "DeepSeek",
    keyringAccount: "deepseek-api-key",
    keyPrefix: "sk-",
    consoleUrl: "https://platform.deepseek.com/api_keys",
  },
  {
    id: "mistral",
    label: "Mistral",
    keyringAccount: "mistral-api-key",
    keyPrefix: null,
    consoleUrl: "https://console.mistral.ai/api-keys/",
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    keyringAccount: "openrouter-api-key",
    keyPrefix: "sk-or-",
    consoleUrl: "https://openrouter.ai/keys",
  },
  {
    id: "openai-compatible",
    label: "OpenAI Compatible",
    keyringAccount: "openai-compatible-api-key",
    keyPrefix: null,
    consoleUrl: "https://platform.openai.com/docs/api-reference",
    keyOptional: true,
  },
  {
    id: "lmstudio",
    label: "LM Studio",
    keyringAccount: "",
    keyPrefix: null,
    consoleUrl: "https://lmstudio.ai/docs/basics/server",
  },
  {
    id: "mlx",
    label: "MLX",
    keyringAccount: "",
    keyPrefix: null,
    consoleUrl:
      "https://github.com/ml-explore/mlx-lm/blob/main/mlx_lm/SERVER.md",
  },
  {
    id: "ollama",
    label: "Ollama",
    keyringAccount: "",
    keyPrefix: null,
    consoleUrl: "https://ollama.com/download",
  },
] as const;

export type MessageProtocol = "chat_completions" | "responses" | "messages";

export type CustomModel = {
  id: string;
  enabled?: boolean;
  /** 智能模式只保存手工覆盖值，推荐值随模型目录变化。 */
  useRecommendedConfig?: boolean;
  contextLimit?: number;
  maxOutputTokens?: number;
  vision?: boolean;
  reasoningLevels?: string[];
  reasoningLevelMap?: ReasoningLevelMap;
};

export type CustomEndpoint = {
  id: string;
  name: string;
  baseURL: string;
  modelId: string;
  contextLimit: number;
  /** 老配置未保存协议时继续使用 Chat Completions。 */
  protocol?: MessageProtocol;
  /** 未提供列表的旧配置仍通过 modelId 和 contextLimit 读取。 */
  models?: CustomModel[];
};

const COMPAT_MODEL_PREFIX = "compat-";

export function compatModelIdForEndpoint(
  endpointId: string,
  apiModelId?: string,
): string {
  return `${COMPAT_MODEL_PREFIX}${endpointId}${apiModelId ? `/${encodeURIComponent(apiModelId)}` : ""}`;
}

export function isCompatModelId(modelId: string): boolean {
  return modelId.startsWith(COMPAT_MODEL_PREFIX);
}

export function endpointIdFromCompatModel(modelId: string): string {
  return isCompatModelId(modelId)
    ? modelId.slice(COMPAT_MODEL_PREFIX.length).split("/")[0]
    : "";
}

export function endpointModels(
  endpoint: CustomEndpoint,
): readonly CustomModel[] {
  if (endpoint.models !== undefined) return endpoint.models;
  return endpoint.modelId.trim()
    ? [
        {
          id: endpoint.modelId.trim(),
          contextLimit: endpoint.contextLimit,
          vision: true,
          useRecommendedConfig: false,
        },
      ]
    : [];
}

export function endpointModelSelectionId(
  endpoint: CustomEndpoint,
  model: CustomModel,
): string {
  return compatModelIdForEndpoint(
    endpoint.id,
    endpoint.models === undefined ? undefined : model.id,
  );
}

/** 显式选择已删除的模型时返回空，禁止悄悄发送给同一供应商的另一个模型。 */
export function resolveEndpointModel(
  modelId: string,
  endpoints: readonly CustomEndpoint[],
): { endpoint: CustomEndpoint; model: CustomModel } | undefined {
  const endpoint = endpoints.find(
    (e) => e.id === endpointIdFromCompatModel(modelId),
  );
  if (!endpoint) return;
  const encoded = modelId
    .slice(COMPAT_MODEL_PREFIX.length)
    .split("/")
    .slice(1)
    .join("/");
  let id = endpoint.modelId;
  try {
    if (encoded) id = decodeURIComponent(encoded);
  } catch {
    return;
  }
  const model = endpointModels(endpoint).find((m) => m.id === id);
  return model ? { endpoint, model } : undefined;
}

export function recommendedModelConfig(id: string, baseURL?: string) {
  const catalogId = id.trim();
  const segments = catalogId.split("/");
  const known: ModelInfo | undefined =
    MODELS.find((model) => model.id === catalogId) ??
    MODELS.find((model) => model.id === segments[segments.length - 1]);
  const remote = catalogModelRecommendation(catalogId, baseURL);
  const contextLimit =
    remote?.contextLimit ?? getModelContextLimit(known?.id ?? catalogId);
  return {
    known: !!remote || !!known,
    source: remote ? "models.dev" : known ? "bundled" : "default",
    outputLimit: remote?.outputLimit ?? undefined,
    contextLimit,
    // 与原生 Runner 的窗口四分之一、32,000 上限保持一致。
    maxOutputTokens: Math.min(
      Math.floor(contextLimit / 4),
      32_000,
      remote?.outputLimit ?? Infinity,
    ),
    vision: remote?.vision ?? known?.tags?.includes("vision") ?? false,
    reasoning: remote?.reasoning ?? known?.tags?.includes("reasoning") ?? false,
    reasoningLevels:
      remote?.reasoningLevels ??
      recommendedReasoningLevels(
        catalogId,
        remote?.reasoning ?? known?.tags?.includes("reasoning") ?? false,
      ),
  };
}

export function effectiveCustomModel(model: CustomModel, baseURL?: string) {
  const recommended = recommendedModelConfig(model.id, baseURL);
  const contextLimit = model.contextLimit ?? recommended.contextLimit;
  return {
    ...recommended,
    contextLimit,
    maxOutputTokens:
      model.maxOutputTokens ??
      Math.min(
        Math.floor(contextLimit / 4),
        32_000,
        recommended.outputLimit ?? Infinity,
      ),
    vision: model.vision ?? recommended.vision,
    reasoningLevels:
      model.reasoningLevels === undefined
        ? recommended.reasoningLevels
        : validateReasoningLevels(model.reasoningLevels),
    reasoningLevelMap: model.reasoningLevelMap,
  };
}

/** One-shot migration of the legacy single OpenAI-compatible config into the
 *  named-endpoint list. Returns one endpoint when the old base URL + model id
 *  were both set, else empty. `id` is supplied by the caller to stay pure. */
export function migrateLegacyCompatEndpoint(
  baseURL: string,
  modelId: string,
  contextLimit: number,
  id: string,
): CustomEndpoint[] {
  if (!baseURL.trim() || !modelId.trim()) return [];
  return [{ id, name: "Custom endpoint", baseURL, modelId, contextLimit }];
}

export function getProvider(id: ProviderId): ProviderInfo {
  const p = PROVIDERS.find((x) => x.id === id);
  if (!p) throw new Error(`Unknown provider: ${id}`);
  return p;
}

/** 1 (lowest) – 5 (highest). For `cost`, higher = cheaper. */
export type CapabilityScore = 1 | 2 | 3 | 4 | 5;

export type ModelCapabilities = {
  intelligence: CapabilityScore;
  speed: CapabilityScore;
  cost: CapabilityScore;
};

export type ModelTag = "vision" | "reasoning" | "tools" | "coding";

export type ModelInfo = {
  id: string;
  provider: ProviderId;
  label: string;
  capabilities: ModelCapabilities;
  tags?: readonly ModelTag[];
  supportsTemperature?: boolean;
};

export const MODELS = [
  // ── OpenAI ────────────────────────────────────────────────────────────────
  {
    id: "gpt-5.6",
    provider: "openai",
    label: "GPT-5.6 Sol",
    capabilities: { intelligence: 5, speed: 4, cost: 1 },
    tags: ["vision", "reasoning", "tools", "coding"],
    supportsTemperature: false,
  },
  {
    id: "gpt-5.6-terra",
    provider: "openai",
    label: "GPT-5.6 Terra",
    capabilities: { intelligence: 5, speed: 4, cost: 2 },
    tags: ["vision", "reasoning", "tools", "coding"],
    supportsTemperature: false,
  },
  {
    id: "gpt-5.6-luna",
    provider: "openai",
    label: "GPT-5.6 Luna",
    capabilities: { intelligence: 4, speed: 5, cost: 3 },
    tags: ["vision", "reasoning", "tools", "coding"],
    supportsTemperature: false,
  },
  {
    id: "gpt-5.5",
    provider: "openai",
    label: "GPT-5.5",
    capabilities: { intelligence: 5, speed: 3, cost: 1 },
    tags: ["vision", "reasoning", "tools", "coding"],
    supportsTemperature: false,
  },
  {
    id: "gpt-5.5-pro",
    provider: "openai",
    label: "GPT-5.5 Pro",
    capabilities: { intelligence: 5, speed: 2, cost: 1 },
    tags: ["vision", "reasoning", "tools", "coding"],
    supportsTemperature: false,
  },
  {
    id: "gpt-5.4-mini",
    provider: "openai",
    label: "GPT-5.4 mini",
    capabilities: { intelligence: 4, speed: 4, cost: 4 },
    tags: ["vision", "tools"],
    supportsTemperature: false,
  },
  {
    id: "gpt-5.4-nano",
    provider: "openai",
    label: "GPT-5.4 nano",
    capabilities: { intelligence: 3, speed: 5, cost: 5 },
    tags: ["tools"],
    supportsTemperature: false,
  },
  {
    id: "gpt-5.3-codex",
    provider: "openai",
    label: "GPT-5.3 Codex",
    capabilities: { intelligence: 4, speed: 4, cost: 3 },
    tags: ["tools", "coding"],
    supportsTemperature: false,
  },
  {
    id: "gpt-4.1-mini",
    provider: "openai",
    label: "GPT-4.1 mini",
    capabilities: { intelligence: 3, speed: 4, cost: 5 },
    tags: ["vision", "tools"],
  },

  // ── Anthropic ─────────────────────────────────────────────────────────────
  {
    id: "claude-fable-5",
    provider: "anthropic",
    label: "Claude Fable 5",
    capabilities: { intelligence: 5, speed: 2, cost: 1 },
    tags: ["vision", "reasoning", "tools", "coding"],
    supportsTemperature: false,
  },
  {
    id: "claude-sonnet-5",
    provider: "anthropic",
    label: "Claude Sonnet 5",
    capabilities: { intelligence: 5, speed: 4, cost: 3 },
    tags: ["vision", "reasoning", "tools", "coding"],
    supportsTemperature: false,
  },
  {
    id: "claude-opus-4-8",
    provider: "anthropic",
    label: "Claude Opus 4.8",
    capabilities: { intelligence: 5, speed: 2, cost: 1 },
    tags: ["vision", "reasoning", "tools", "coding"],
    supportsTemperature: false,
  },
  {
    id: "claude-opus-4-7",
    provider: "anthropic",
    label: "Claude Opus 4.7",
    capabilities: { intelligence: 5, speed: 2, cost: 1 },
    tags: ["vision", "reasoning", "tools", "coding"],
    supportsTemperature: false,
  },
  {
    id: "claude-sonnet-4-6",
    provider: "anthropic",
    label: "Claude Sonnet 4.6",
    capabilities: { intelligence: 4, speed: 4, cost: 3 },
    tags: ["vision", "tools", "coding"],
  },
  {
    id: "claude-haiku-4-5",
    provider: "anthropic",
    label: "Claude Haiku 4.5",
    capabilities: { intelligence: 3, speed: 5, cost: 4 },
    tags: ["vision", "tools"],
  },
  {
    id: "claude-opus-4-6",
    provider: "anthropic",
    label: "Claude Opus 4.6",
    capabilities: { intelligence: 5, speed: 2, cost: 1 },
    tags: ["vision", "reasoning", "tools", "coding"],
  },

  // ── xAI ───────────────────────────────────────────────────────────────────
  {
    id: "grok-4.5",
    provider: "xai",
    label: "Grok 4.5",
    capabilities: { intelligence: 5, speed: 4, cost: 2 },
    tags: ["vision", "reasoning", "tools", "coding"],
  },
  {
    id: "grok-4.20-reasoning",
    provider: "xai",
    label: "Grok 4.20 Reasoning",
    capabilities: { intelligence: 5, speed: 2, cost: 2 },
    tags: ["reasoning", "tools", "coding"],
  },
  {
    id: "grok-4.20-non-reasoning",
    provider: "xai",
    label: "Grok 4.20",
    capabilities: { intelligence: 4, speed: 4, cost: 3 },
    tags: ["tools"],
  },
  {
    id: "grok-4-fast-reasoning",
    provider: "xai",
    label: "Grok 4 Fast",
    capabilities: { intelligence: 4, speed: 4, cost: 4 },
    tags: ["vision", "reasoning", "tools"],
  },
  {
    id: "grok-4.3",
    provider: "xai",
    label: "Grok 4.3",
    capabilities: { intelligence: 5, speed: 4, cost: 2 },
    tags: ["vision", "reasoning", "tools", "coding"],
  },
  {
    id: "grok-build-0.1",
    provider: "xai",
    label: "Grok Build 0.1",
    capabilities: { intelligence: 4, speed: 5, cost: 4 },
    tags: ["tools", "coding"],
  },

  // ── DeepSeek ──────────────────────────────────────────────────────────────
  {
    id: "deepseek-flash",
    provider: "deepseek",
    label: "DeepSeek V4.1 Flash",
    capabilities: { intelligence: 4, speed: 5, cost: 5 },
    tags: ["vision", "reasoning", "tools", "coding"],
  },
  {
    id: "deepseek-v4.1-flash",
    provider: "deepseek",
    label: "DeepSeek V4.1 Flash",
    capabilities: { intelligence: 4, speed: 5, cost: 5 },
    tags: ["vision", "reasoning", "tools", "coding"],
  },
  {
    id: "deepseek-v4-pro",
    provider: "deepseek",
    label: "DeepSeek V4 Pro",
    capabilities: { intelligence: 5, speed: 3, cost: 4 },
    tags: ["reasoning", "tools", "coding"],
  },
  {
    id: "deepseek-v4-flash",
    provider: "deepseek",
    label: "DeepSeek V4 Flash",
    capabilities: { intelligence: 4, speed: 5, cost: 5 },
    tags: ["reasoning", "tools"],
  },
  {
    id: "deepseek-reasoner",
    provider: "deepseek",
    label: "DeepSeek Reasoner",
    capabilities: { intelligence: 5, speed: 2, cost: 4 },
    tags: ["reasoning", "coding"],
  },

  // ── Mistral ────────────────────────────────────────────────────────────────
  {
    id: "mistral-large-latest",
    provider: "mistral",
    label: "Mistral Large 3",
    capabilities: { intelligence: 5, speed: 3, cost: 3 },
    tags: ["vision", "tools", "coding"],
  },
  {
    id: "mistral-medium-latest",
    provider: "mistral",
    label: "Mistral Medium 3.5",
    capabilities: { intelligence: 4, speed: 4, cost: 4 },
    tags: ["vision", "tools"],
  },
  {
    id: "codestral-latest",
    provider: "mistral",
    label: "Codestral",
    capabilities: { intelligence: 4, speed: 4, cost: 4 },
    tags: ["coding"],
  },

  // ── Cerebras (autocomplete-tier) ──────────────────────────────────────────
  {
    id: "gpt-oss-120b",
    provider: "cerebras",
    label: "GPT-OSS 120B",
    capabilities: { intelligence: 4, speed: 5, cost: 4 },
    tags: ["tools", "coding"],
  },
  {
    id: "llama3.3-70b",
    provider: "cerebras",
    label: "Llama 3.3 70B",
    capabilities: { intelligence: 3, speed: 5, cost: 5 },
    tags: ["tools"],
  },
  {
    id: "qwen-3-32b",
    provider: "cerebras",
    label: "Qwen 3 32B",
    capabilities: { intelligence: 3, speed: 5, cost: 5 },
    tags: ["tools", "coding"],
  },

  // ── Groq (autocomplete-tier) ──────────────────────────────────────────────
  {
    id: "openai/gpt-oss-20b",
    provider: "groq",
    label: "GPT-OSS 20B",
    capabilities: { intelligence: 3, speed: 5, cost: 5 },
    tags: ["tools", "coding"],
  },
  {
    id: "llama-3.3-70b-versatile",
    provider: "groq",
    label: "Llama 3.3 70B",
    capabilities: { intelligence: 4, speed: 5, cost: 5 },
    tags: ["tools"],
  },
  {
    id: "deepseek-r1-distill-llama-70b",
    provider: "groq",
    label: "DeepSeek R1 Distill 70B",
    capabilities: { intelligence: 4, speed: 5, cost: 5 },
    tags: ["reasoning", "tools"],
  },

  // ── OpenRouter (gateway; model id is user-supplied at runtime) ────────────
  {
    id: "openrouter-custom",
    provider: "openrouter",
    label: "OpenRouter",
    capabilities: { intelligence: 3, speed: 3, cost: 3 },
  },

  // ── Generic OpenAI-compatible (user-defined endpoint) ─────────────────────
  {
    id: "openai-compatible-custom",
    provider: "openai-compatible",
    label: "Custom endpoint",
    capabilities: { intelligence: 3, speed: 3, cost: 3 },
  },

  // ── LM Studio (local; model id is user-supplied at runtime) ───────────────
  {
    id: "lmstudio-local",
    provider: "lmstudio",
    label: "LM Studio",
    capabilities: { intelligence: 3, speed: 3, cost: 5 },
  },

  // ── MLX (local; Apple-silicon; model id is user-supplied at runtime) ──────
  {
    id: "mlx-local",
    provider: "mlx",
    label: "MLX",
    capabilities: { intelligence: 3, speed: 3, cost: 5 },
  },

  // ── Ollama (local; model id is user-supplied at runtime) ──────────────────
  {
    id: "ollama-local",
    provider: "ollama",
    label: "Ollama",
    capabilities: { intelligence: 3, speed: 3, cost: 5 },
  },
] as const satisfies readonly ModelInfo[];

export type ModelId = (typeof MODELS)[number]["id"];

export function getCompatModelInfo(
  modelId: string,
  endpoints: readonly CustomEndpoint[],
): ModelInfo {
  const eid = endpointIdFromCompatModel(modelId);
  const ep = endpoints.find((e) => e.id === eid);
  const custom = resolveEndpointModel(modelId, endpoints)?.model;
  const effective = custom
    ? effectiveCustomModel(custom, ep?.baseURL)
    : undefined;
  const name = ep?.name || "Custom endpoint";
  return {
    id: modelId,
    provider: "openai-compatible",
    label: custom?.id || ep?.modelId || name,
    capabilities: { intelligence: 3, speed: 3, cost: 3 },
    tags: effective
      ? [
          ...(effective.vision ? ["vision" as const] : []),
          ...(effective.reasoning ? ["reasoning" as const] : []),
        ]
      : undefined,
  };
}

export function resolveModel(
  modelId: string,
  endpoints: readonly CustomEndpoint[] = [],
): ModelInfo {
  if (isCompatModelId(modelId)) return getCompatModelInfo(modelId, endpoints);
  const m = MODELS.find((x) => x.id === modelId);
  if (!m) throw new Error(`Unknown model: ${modelId}`);
  return m;
}

export function getModel(id: ModelId): ModelInfo {
  const m = MODELS.find((x) => x.id === id);
  if (!m) throw new Error(`Unknown model: ${id}`);
  return m;
}

export function isKnownModelId(id: string): id is ModelId {
  return MODELS.some((x) => x.id === id);
}

const FREEFORM_PROVIDERS: ReadonlySet<ProviderId> = new Set([
  "openrouter",
  "openai-compatible",
  "lmstudio",
  "mlx",
  "ollama",
]);

// Reasoning models reject tool-call turns whose reasoning was stripped; keep it.
export function modelKeepsReasoning(m: ModelInfo): boolean {
  return (
    (m.tags?.includes("reasoning") ?? false) ||
    FREEFORM_PROVIDERS.has(m.provider)
  );
}

export function modelSupportsTemperature(
  provider: ProviderId,
  modelId: string,
): boolean {
  const model: ModelInfo | undefined = MODELS.find(
    (m) => m.provider === provider && m.id === modelId,
  );
  return model?.supportsTemperature !== false;
}

export function modelUsesReasoningTokens(
  provider: ProviderId,
  modelId: string,
): boolean {
  const model: ModelInfo | undefined = MODELS.find(
    (m) => m.provider === provider && m.id === modelId,
  );
  return (
    (model?.tags?.includes("reasoning") ?? false) ||
    (provider === "openai" && /^gpt-5(?:[.-]|$)/.test(modelId)) ||
    /\bgpt-oss\b/i.test(modelId)
  );
}

export const DEFAULT_MODEL_ID: ModelId = "gpt-5.4-mini";

/** Approximate context window (in tokens) per model. Used for the
 *  context-usage indicator in the AI mini-window header. Conservative
 *  estimates : actual provider limits may shift. */
export const MODEL_CONTEXT_LIMITS: Record<string, number> = {
  "gpt-5.6": 1_050_000,
  "gpt-5.6-terra": 1_050_000,
  "gpt-5.6-luna": 1_050_000,
  "gpt-5.5": 1_050_000,
  "gpt-5.5-pro": 1_050_000,
  "gpt-5.4-mini": 400_000,
  "gpt-5.4-nano": 400_000,
  "gpt-5.3-codex": 400_000,
  "gpt-4.1-mini": 128_000,
  "claude-fable-5": 1_000_000,
  "claude-sonnet-5": 1_000_000,
  "claude-opus-4-7": 1_000_000,
  "claude-opus-4-8": 1_000_000,
  "claude-sonnet-4-6": 1_000_000,
  "claude-haiku-4-5": 200_000,
  "claude-opus-4-6": 1_000_000,
  "grok-4.5": 500_000,
  "grok-4.20-reasoning": 2_000_000,
  "grok-4.20-non-reasoning": 2_000_000,
  "grok-4-fast-reasoning": 2_000_000,
  "grok-4.3": 1_000_000,
  "grok-build-0.1": 256_000,
  "deepseek-flash": 1_000_000,
  "deepseek-v4.1-flash": 1_000_000,
  "deepseek-v4-pro": 1_000_000,
  "deepseek-v4-flash": 1_000_000,
  "deepseek-reasoner": 128_000,
  "gpt-oss-120b": 128_000,
  "llama3.3-70b": 128_000,
  "qwen-3-32b": 32_000,
  "openai/gpt-oss-20b": 128_000,
  "llama-3.3-70b-versatile": 128_000,
  "deepseek-r1-distill-llama-70b": 128_000,
  "openrouter-custom": 256_000,
  "openai-compatible-custom": 128_000,
  "lmstudio-local": 32_000,
  "mlx-local": 32_000,
  "ollama-local": 32_000,
  "mistral-large-latest": 131_072,
  "mistral-medium-latest": 32_768,
  "codestral-latest": 256_000,
};

export function getModelContextLimit(
  modelId: string | undefined,
  compatOverride?: number,
): number {
  if (!modelId) return 128_000;
  if (isCompatModelId(modelId)) return compatOverride ?? 128_000;
  if (modelId === "openai-compatible-custom" && compatOverride)
    return compatOverride;
  return MODEL_CONTEXT_LIMITS[modelId] ?? 128_000;
}

export type ModelPricing = {
  input: number;
  output: number;
  cacheRead?: number;
};

export const MODEL_PRICING: Record<string, ModelPricing> = {
  "gpt-5.6": { input: 5, output: 30, cacheRead: 0.5 },
  "gpt-5.6-terra": { input: 2.5, output: 15, cacheRead: 0.25 },
  "gpt-5.6-luna": { input: 1, output: 6, cacheRead: 0.1 },
  "gpt-5.5": { input: 5, output: 30, cacheRead: 0.5 },
  "gpt-5.5-pro": { input: 30, output: 180 },
  "gpt-5.4-mini": { input: 0.75, output: 4.5, cacheRead: 0.075 },
  "gpt-5.4-nano": { input: 0.2, output: 1.25, cacheRead: 0.02 },
  "gpt-5.3-codex": { input: 1.5, output: 6, cacheRead: 0.15 },
  "gpt-4.1-mini": { input: 0.4, output: 1.6, cacheRead: 0.1 },
  "claude-fable-5": { input: 10, output: 50, cacheRead: 1 },
  "claude-sonnet-5": { input: 3, output: 15, cacheRead: 0.3 },
  "claude-opus-4-7": { input: 15, output: 75, cacheRead: 1.5 },
  "claude-opus-4-8": { input: 5, output: 25, cacheRead: 0.5 },
  "claude-opus-4-6": { input: 15, output: 75, cacheRead: 1.5 },
  "claude-sonnet-4-6": { input: 3, output: 15, cacheRead: 0.3 },
  "claude-haiku-4-5": { input: 1, output: 5, cacheRead: 0.1 },
  "grok-4.5": { input: 2, output: 6, cacheRead: 0.5 },
  "grok-4.20-reasoning": { input: 3, output: 15 },
  "grok-4.20-non-reasoning": { input: 1, output: 5 },
  "grok-4-fast-reasoning": { input: 0.2, output: 0.5 },
  "grok-4.3": { input: 1.25, output: 2.5 },
  "grok-build-0.1": { input: 1, output: 2 },
  "deepseek-v4-pro": { input: 0.28, output: 1.1, cacheRead: 0.028 },
  "deepseek-v4-flash": { input: 0.07, output: 0.27, cacheRead: 0.007 },
  "deepseek-reasoner": { input: 0.55, output: 2.19, cacheRead: 0.14 },
};

export function estimateCost(
  modelId: string | undefined,
  usage: {
    inputTokens: number;
    outputTokens: number;
    cachedInputTokens: number;
  },
): number | null {
  if (!modelId) return null;
  const p = MODEL_PRICING[modelId];
  if (!p) return null;
  const fresh = Math.max(0, usage.inputTokens - usage.cachedInputTokens);
  const cached = usage.cachedInputTokens;
  return (
    (fresh * p.input +
      cached * (p.cacheRead ?? p.input) +
      usage.outputTokens * p.output) /
    1_000_000
  );
}

/** Providers that do not require an API key (local servers, key-optional). */
export const KEYLESS_PROVIDERS: readonly ProviderId[] = [
  "lmstudio",
  "mlx",
  "ollama",
  "openai-compatible",
] as const;

export function providerNeedsKey(id: ProviderId): boolean {
  return !KEYLESS_PROVIDERS.includes(id);
}

/** True for providers that accept an API key : required *or* optional.
 *  Used by Settings to decide whether to render a key card at all. */
export function providerSupportsKey(id: ProviderId): boolean {
  if (providerNeedsKey(id)) return true;
  const p = getProvider(id);
  return !!p.keyOptional;
}

/** Any provider can power the editor's inline autocomplete; latency is the
 *  user's choice. The picker filters down to fast tiers in the UI. */
export type AutocompleteProviderId = ProviderId;

/** Sensible default model id per provider for inline autocomplete. */
export const DEFAULT_AUTOCOMPLETE_MODEL: Partial<Record<ProviderId, string>> = {
  cerebras: "gpt-oss-120b",
  groq: "openai/gpt-oss-20b",
  lmstudio: "qwen2.5-coder-7b-instruct",
  openai: "gpt-5.4-nano",
  anthropic: "claude-haiku-4-5",
  xai: "grok-4.3",
  deepseek: "deepseek-v4-flash",
  openrouter: "openai/gpt-5.4-mini",
  "openai-compatible": "",
};

/** Curated list of fast models suitable for inline completion (speed ≥ 4). */
export function getAutocompleteEligibleModels(): readonly ModelInfo[] {
  return MODELS.filter(
    (m) => m.capabilities.speed >= 4 && m.id !== "openai-compatible-custom",
  );
}

export type SttProvider = "openai" | "groq" | "whispercpp";

export const STT_PROVIDER_LABELS: Record<SttProvider, string> = {
  openai: "OpenAI Whisper",
  groq: "Groq Whisper",
  whispercpp: "Whisper.cpp (local)",
};

export const DEFAULT_STT_PROVIDER: SttProvider = "openai";
export const WHISPERCPP_DEFAULT_BASE_URL = "http://127.0.0.1:8080";
export const LMSTUDIO_DEFAULT_BASE_URL = "http://localhost:1234/v1";
export const MLX_DEFAULT_BASE_URL = "http://127.0.0.1:8080/v1";
export const OLLAMA_DEFAULT_BASE_URL = "http://localhost:11434/v1";
export const OPENAI_COMPATIBLE_DEFAULT_BASE_URL = "";
export const MAX_AGENT_STEPS = 24;
export const TERMINAL_BUFFER_LINES = 300;

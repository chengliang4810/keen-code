/** 当前供应商模型与推理强度展示目录。 */

import type { ModelMetadata, ModelReasoningInfo } from "./api";

export interface EffortOption {
  /** 传给当前模型供应商的推理强度标识。 */
  id: string;
  /** 传给供应商的值；未提供时与 id 相同。 */
  value?: string;
  /** Display label from catalog when present. */
  label?: string;
  description?: string;
  isDefault?: boolean;
}

export interface ModelOption {
  /** 供应商内部稳定标识。 */
  providerId?: string;
  /** 供应商展示名称。 */
  providerLabel?: string;
  id: string;
  /** Display name (language-neutral product name) */
  label: string;
  /** 当前供应商是否将该模型标记为默认模型。 */
  isDefault?: boolean;
  /** Catalog source; composer only shows official model IDs (not providers). */
  source?: string;
  /** 当前模型明确支持的推理强度；空值或空数组表示未知或不可调。 */
  reasoningEfforts?: EffortOption[];
  /** 远端目录是否明确声明支持推理；空值表示未知。 */
  reasoningSupported?: boolean;
  /** 远端目录返回的上下文窗口 token 数。 */
  contextWindow?: number;
  /** 远端目录返回的最大输出 token 数。 */
  maxOutputTokens?: number;
}

/**
 * Default reasoning depth. `medium` balances speed vs quality for agentic use;
 * users can lower (faster) or raise (deeper) via the composer chip.
 * When a model lists a default effort, prefer `pickDefaultEffort(model)`.
 */
export const DEFAULT_EFFORT = "medium";

export function isValidModelId(
  id: string,
  catalog: ModelOption[],
): boolean {
  return catalog.some((m) => m.id === id);
}

/** 当前供应商必须存在，并且其活动模型仍在可用模型目录中。 */
export function hasConfiguredProviderModel(
  providerId: string | null | undefined,
  modelId: string | null | undefined,
  catalog: ModelOption[],
): boolean {
  if (!providerId || !modelId) return false;
  return catalog.some(
    (model) => model.providerId === providerId && model.id === modelId,
  );
}

/**
 * Efforts list for a model: live catalog when non-empty, else static fallback.
 */
export function effortsForModel(
  model?: ModelOption | null,
  catalogEfforts?: EffortOption[] | null,
): EffortOption[] {
  const fromArg =
    catalogEfforts && catalogEfforts.length > 0 ? catalogEfforts : null;
  const fromModel =
    model?.reasoningEfforts && model.reasoningEfforts.length > 0
      ? model.reasoningEfforts
      : null;
  return fromArg ?? fromModel ?? [];
}

/**
 * 只在远端目录明确返回的推理强度中校验，未知模型不猜测静态档位。
 */
export function isValidEffort(
  id: string,
  modelOrEfforts?: ModelOption | EffortOption[] | null,
): boolean {
  if (!id) return false;
  if (Array.isArray(modelOrEfforts)) {
    return effortsForModel(null, modelOrEfforts).some((e) => e.id === id);
  }
  return effortsForModel(modelOrEfforts).some((e) => e.id === id);
}

/** Default effort for a model (catalog default flag, else first, else medium). */
export function pickDefaultEffort(
  model?: ModelOption | null,
  catalogEfforts?: EffortOption[] | null,
): string {
  const list = effortsForModel(model, catalogEfforts);
  return (
    list.find((e) => e.isDefault)?.id ?? list[0]?.id ?? DEFAULT_EFFORT
  );
}

/** 将远端推理信息转换为 Composer 可直接展示的强度列表。 */
export function reasoningEffortsFromMetadata(
  reasoning?: ModelReasoningInfo | null,
): EffortOption[] {
  if (!reasoning?.supported) return [];
  const effort = reasoning.controls.find((control) => control.type === "effort");
  if (!effort || effort.type !== "effort") return [];
  return effort.values.map((id) => ({
    id,
    value: id,
    isDefault: reasoning.defaultEffort === id,
  }));
}

/** 将单模型元数据投影到供应商模型菜单，不改变供应商路由信息。 */
export function applyModelMetadata(
  model: ModelOption,
  metadata?: ModelMetadata | null,
): ModelOption {
  if (!metadata || metadata.modelId !== model.id) return model;
  return {
    ...model,
    reasoningSupported: metadata.reasoning?.supported,
    reasoningEfforts: reasoningEffortsFromMetadata(metadata.reasoning),
    contextWindow: metadata.contextWindow ?? undefined,
    maxOutputTokens: metadata.maxOutputTokens ?? undefined,
  };
}

/**
 * Strip a shared suffix so "High Effort" / "Medium Effort" collapse to
 * "High" / "Medium" (identical trailing " Effort" is noise in compact UI).
 */
export function stripCommonEffortSuffix(label: string): string {
  const trimmed = label.trim();
  if (!trimmed) return trimmed;
  const stripped = trimmed.replace(/\s+Effort$/i, "").trim();
  return stripped || trimmed;
}

/**
 * Display label for an effort.
 * - Standard ids (`high` / `medium` / `low`): prefer i18n so locale controls
 *   高/中/低 vs High/Medium/Low (catalog labels are English-only).
 * - Other catalog labels: strip a shared " Effort" suffix, then raw id.
 */
export function effortDisplayLabel(
  effort: EffortOption | string,
  i18nLabels?: {
    none?: string;
    minimal?: string;
    high?: string;
    medium?: string;
    low?: string;
    xhigh?: string;
    max?: string;
  },
): string {
  const id = typeof effort === "string" ? effort : effort.id;
  if (id === "none" && i18nLabels?.none) return i18nLabels.none;
  if (id === "minimal" && i18nLabels?.minimal) return i18nLabels.minimal;
  if (id === "high" && i18nLabels?.high) return i18nLabels.high;
  if (id === "medium" && i18nLabels?.medium) return i18nLabels.medium;
  if (id === "low" && i18nLabels?.low) return i18nLabels.low;
  if (id === "xhigh" && i18nLabels?.xhigh) return i18nLabels.xhigh;
  if (id === "max" && i18nLabels?.max) return i18nLabels.max;

  if (typeof effort !== "string") {
    const raw = effort.label?.trim();
    if (raw) return stripCommonEffortSuffix(raw);
    return effortDisplayLabel(effort.id, i18nLabels);
  }
  return effort;
}

/** Find a model in catalog by id. */
export function findModel(
  id: string,
  catalog: ModelOption[],
): ModelOption | undefined {
  return catalog.find((m) => m.id === id);
}

/**
 * 为新会话挑选默认模型：优先当前供应商标记为默认的模型（默认标识与
 * defaultModel 一致时），否则回退到当前供应商的第一个模型；
 * 当前供应商不在目录中时返回 undefined。
 */
export function pickNewChatModel(
  activeProviderId: string | null,
  defaultModel: string | null,
  models: ModelOption[],
): ModelOption | undefined {
  const providerModels = models.filter(
    (model) => model.providerId === activeProviderId,
  );
  return (
    providerModels.find(
      (model) =>
        model.isDefault && (defaultModel === null || model.id === defaultModel),
    ) ?? providerModels[0]
  );
}

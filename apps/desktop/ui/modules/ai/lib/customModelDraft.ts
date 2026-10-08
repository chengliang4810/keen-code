import {
  parseReasoningLevelMap,
  validateReasoningLevels,
} from "@/modules/ai/lib/reasoning";
import { effectiveCustomModel, type CustomModel } from "@/modules/ai/config";

export type CustomModelDraft = {
  id: string;
  smart: boolean;
  context: string;
  output: string;
  vision: boolean | null;
  reasoningLevels: string[] | null;
  reasoningMap: string;
};

export function createCustomModelDraft(model?: CustomModel): CustomModelDraft {
  return {
    id: model?.id ?? "",
    smart: model?.useRecommendedConfig !== false,
    context:
      model?.contextLimit === undefined ? "" : String(model.contextLimit),
    output:
      model?.maxOutputTokens === undefined ? "" : String(model.maxOutputTokens),
    vision: model?.vision ?? null,
    reasoningLevels: model?.reasoningLevels ?? null,
    reasoningMap:
      model?.reasoningLevelMap === undefined
        ? ""
        : JSON.stringify(model.reasoningLevelMap, null, 2),
  };
}

export function restoreCustomModelDraft(
  draft: CustomModelDraft,
): CustomModelDraft {
  return {
    id: draft.id,
    smart: true,
    context: "",
    output: "",
    vision: null,
    reasoningLevels: null,
    reasoningMap: "",
  };
}

function tokenValue(
  value: string,
  min: number,
  max: number,
): number | undefined {
  if (!value.trim()) return;
  const number = Number(value);
  if (
    !/^\d+$/.test(value.trim()) ||
    !Number.isSafeInteger(number) ||
    number < min ||
    number > max
  )
    throw new Error(
      "Token limits must be valid whole numbers within the supported range.",
    );
  return number;
}

function draftModelValues(
  draft: CustomModelDraft,
  enabled = true,
  baseURL?: string,
): CustomModel {
  const id = draft.id.trim();
  const model: CustomModel = {
    id,
    enabled,
    useRecommendedConfig: draft.smart,
    contextLimit: tokenValue(draft.context, 8192, 4_000_000),
    maxOutputTokens: tokenValue(draft.output, 1, 1_000_000),
    vision: draft.vision ?? undefined,
    reasoningLevels:
      draft.reasoningLevels === null
        ? undefined
        : validateReasoningLevels(draft.reasoningLevels),
    reasoningLevelMap: parseReasoningLevelMap(draft.reasoningMap),
  };
  const effective = effectiveCustomModel(model, baseURL);
  if (effective.maxOutputTokens > effective.contextLimit)
    throw new Error("Maximum output cannot exceed the context window.");
  return model;
}

export function commitCustomModelDraft(
  draft: CustomModelDraft,
  enabled = true,
  baseURL?: string,
): CustomModel {
  const id = draft.id.trim();
  if (!id || id.length > 256 || /[\x00-\x1f\x7f]/.test(id))
    throw new Error("Enter a valid model ID.");
  const model = draftModelValues(draft, enabled, baseURL);
  const effective = effectiveCustomModel(model, baseURL);
  // 手动模式冻结有效值；重新启用智能模式由恢复动作清除覆盖。
  return draft.smart
    ? model
    : {
        ...model,
        contextLimit: effective.contextLimit,
        maxOutputTokens: effective.maxOutputTokens,
        vision: effective.vision,
        reasoningLevels: effective.reasoningLevels,
      };
}

export function toggleCustomModelSmart(
  draft: CustomModelDraft,
  smart: boolean,
  baseURL?: string,
): CustomModelDraft {
  const effective = effectiveCustomModel(
    draftModelValues(draft, true, baseURL),
    baseURL,
  );
  if (smart) return restoreCustomModelDraft(draft);
  return {
    ...draft,
    smart: false,
    context: draft.context || String(effective.contextLimit),
    output: draft.output || String(effective.maxOutputTokens),
    vision: draft.vision ?? effective.vision,
    reasoningLevels: draft.reasoningLevels ?? effective.reasoningLevels,
  };
}

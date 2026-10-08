import type { MessageProtocol } from "@/modules/ai/config";

export type ReasoningBody = Record<string, unknown>;
export type ReasoningLevelMap = Record<string, ReasoningBody>;
export type ReasoningSelection = { modelId: string; level: string };

const fields = new Set([
  "reasoning_effort",
  "reasoning",
  "thinking",
  "enable_thinking",
  "thinking_budget",
  "output_config",
]);
const labels: Record<string, string> = {
  none: "Off",
  disabled: "Off",
  off: "Off",
  enabled: "On",
  on: "On",
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Maximum",
  ultra: "Ultra",
};

export function reasoningLevelLabel(level: string): string {
  return labels[level.toLowerCase()] ?? level;
}

export function validateReasoningLevels(value: unknown): string[] {
  if (
    !Array.isArray(value) ||
    value.length > 16 ||
    value.some(
      (level) =>
        typeof level !== "string" ||
        !level.trim() ||
        level !== level.trim() ||
        level.length > 64 ||
        /[\x00-\x1f\x7f]/.test(level),
    )
  )
    throw new Error("Enter up to 16 unique reasoning levels.");
  if (new Set(value).size !== value.length)
    throw new Error("Enter up to 16 unique reasoning levels.");
  return [...value];
}

export function validateReasoningBody(value: unknown): ReasoningBody {
  if (
    !value ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    JSON.stringify(value).length > 8192 ||
    Object.keys(value).some((key) => !fields.has(key))
  )
    throw new Error("Reasoning mapping may only contain reasoning parameters.");
  const validText = (item: unknown) =>
    typeof item === "string" &&
    item.length > 0 &&
    item.length <= 64 &&
    !/[\x00-\x1f\x7f]/.test(item);
  const validBudget = (item: unknown) =>
    typeof item === "number" &&
    Number.isSafeInteger(item) &&
    item >= 0 &&
    item <= 1_000_000;
  for (const [key, item] of Object.entries(value)) {
    let valid = false;
    if (key === "reasoning_effort") valid = validText(item);
    else if (key === "enable_thinking") valid = typeof item === "boolean";
    else if (key === "thinking_budget") valid = validBudget(item);
    else {
      const allowed =
        key === "thinking"
          ? ["type", "budget_tokens"]
          : key === "output_config"
            ? ["effort"]
            : ["effort", "summary", "max_tokens", "enabled", "exclude"];
      valid =
        !!item &&
        typeof item === "object" &&
        !Array.isArray(item) &&
        Object.entries(item).every(
          ([field, entry]) =>
            allowed.includes(field) &&
            (["budget_tokens", "max_tokens"].includes(field)
              ? validBudget(entry)
              : ["enabled", "exclude"].includes(field)
                ? typeof entry === "boolean"
                : validText(entry)),
        );
    }
    if (!valid)
      throw new Error(
        "Reasoning mapping may only contain reasoning parameters.",
      );
  }
  return value as ReasoningBody;
}

export function parseReasoningLevelMap(
  text: string,
): ReasoningLevelMap | undefined {
  if (!text.trim()) return undefined;
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error("Enter a valid JSON reasoning mapping.");
  }
  if (
    !value ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    text.length > 32768
  )
    throw new Error("Enter a valid JSON reasoning mapping.");
  validateReasoningLevels(Object.keys(value));
  for (const body of Object.values(value)) validateReasoningBody(body);
  return value as ReasoningLevelMap;
}

export function selectedReasoningLevel(
  modelId: string,
  levels: readonly string[],
  selection?: ReasoningSelection,
): string | undefined {
  if (selection?.modelId === modelId)
    return levels.includes(selection.level) ? selection.level : undefined;
  return levels[levels.length - 1];
}

export function reasoningRequestBody(
  protocol: MessageProtocol,
  level: string | undefined,
  mapping?: ReasoningLevelMap,
  maxOutputTokens = 32000,
  model = "",
): ReasoningBody | undefined {
  if (!level) return undefined;
  if (mapping && Object.keys(mapping).includes(level))
    return validateReasoningBody(mapping[level]);
  const off = ["none", "disabled", "off"].includes(level);
  const effort = off
    ? "none"
    : level === "enabled" || level === "on"
      ? "high"
      : level;
  if (protocol === "responses") return { reasoning: { effort } };
  if (protocol === "chat_completions") {
    if (
      /deepseek|qwen|glm|kimi/i.test(model) &&
      ["disabled", "enabled", "off", "on"].includes(level)
    )
      return {
        thinking: { type: off ? "disabled" : "enabled" },
        enable_thinking: !off,
      };
    return { reasoning_effort: effort };
  }
  if (off) return { thinking: { type: "disabled" } };
  if (/claude-(?:opus|sonnet)-4-[6-9]/i.test(model))
    return { thinking: { type: "adaptive" }, output_config: { effort } };
  const budgets: Record<string, number> = {
    minimal: 1024,
    low: 2048,
    medium: 8192,
    high: 16384,
    xhigh: 24576,
    max: 28672,
  };
  if (maxOutputTokens <= 1024)
    throw new Error("Thinking requires more than 1,024 output tokens.");
  return {
    thinking: {
      type: "enabled",
      budget_tokens: Math.min(budgets[effort] ?? 16384, maxOutputTokens - 1),
    },
  };
}

export function recommendedReasoningLevels(
  model: string,
  reasoning: boolean,
): string[] {
  if (!reasoning) return [];
  const id = model.toLowerCase();
  if (/gpt-5\.[45]/.test(id)) return ["none", "low", "medium", "high", "xhigh"];
  if (/gpt-5\.[6-9]/.test(id))
    return ["none", "low", "medium", "high", "xhigh", "max"];
  if (/gpt-5|o[134](?:-|$)/.test(id)) return ["low", "medium", "high"];
  if (/claude-(?:opus|sonnet)-4-[6-9]/.test(id))
    return ["disabled", "low", "medium", "high", "max"];
  if (/claude/.test(id) && reasoning)
    return ["disabled", "low", "medium", "high"];
  if (/deepseek.*reasoner|kimi.*thinking/.test(id)) return ["enabled"];
  if (reasoning && /qwen|glm|deepseek|kimi/.test(id))
    return ["disabled", "enabled"];
  return [];
}

export function withReasoningBody(
  fetcher: typeof fetch,
  body?: ReasoningBody,
): typeof fetch {
  if (!body) return fetcher;
  const patch = validateReasoningBody(body);
  return (input, init) => {
    if (typeof init?.body !== "string")
      throw new Error("Model request body is unavailable.");
    const request = JSON.parse(init.body);
    return fetcher(input, {
      ...init,
      body: JSON.stringify({ ...request, ...patch }),
    });
  };
}

export function isReasoningSelectionReady(
  modelId: string,
  levels: readonly string[],
  selection?: ReasoningSelection,
): boolean {
  return (
    levels.length === 0 ||
    selectedReasoningLevel(modelId, levels, selection) !== undefined
  );
}

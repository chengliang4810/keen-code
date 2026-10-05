export interface DraftSuggestedPromptLocalizedText {
  cn?: string;
  en?: string;
}

export const DRAFT_SUGGESTED_PROMPT_NAVIGATE_AUTOMATIONS = "NAVIGATE:AUTOMATIONS" as const;
export const DRAFT_SUGGESTED_PROMPT_NAVIGATE_AUTOMATIONS_OFFPEAK =
  "NAVIGATE:AUTOMATIONS:OFFPEAK" as const;
export type DraftSuggestedPromptAction =
  | typeof DRAFT_SUGGESTED_PROMPT_NAVIGATE_AUTOMATIONS
  | typeof DRAFT_SUGGESTED_PROMPT_NAVIGATE_AUTOMATIONS_OFFPEAK;

export interface DraftSuggestedPromptItem {
  id: string;
  iconName?: string;
  iconUrl?: string;
  iconStyle?: "plugin";
  label: DraftSuggestedPromptLocalizedText;
  prompt: DraftSuggestedPromptLocalizedText;
  actions?: DraftSuggestedPromptAction[];
  plugin?: { stableId: string; label: DraftSuggestedPromptLocalizedText };
}

/** 推荐 Prompt 由本地工作流 JSON 驱动，Client Scenes 网络目录已移除。 */
export function mapClientScenesToDraftSuggestedPromptItems(_scenes: readonly unknown[]): DraftSuggestedPromptItem[] {
  return [];
}

export function resolveDraftSuggestedPromptText(
  text: DraftSuggestedPromptLocalizedText,
  locale: string,
): string {
  const primary = locale.startsWith("zh") ? text.cn : text.en;
  const fallback = locale.startsWith("zh") ? text.en : text.cn;
  return primary?.trim() || fallback?.trim() || "";
}

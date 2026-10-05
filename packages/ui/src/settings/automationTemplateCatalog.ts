export interface AutomationTemplateLocalizedText {
  cn?: string;
  en?: string;
}

export interface ScheduledAutomationTemplate {
  id: string;
  iconName?: string;
  title: AutomationTemplateLocalizedText;
  description: AutomationTemplateLocalizedText;
  prompt: AutomationTemplateLocalizedText;
  cronExpr: string;
  icon: string;
}

export interface OffPeakAutomationTemplate {
  id: string;
  iconName?: string;
  title: AutomationTemplateLocalizedText;
  description: AutomationTemplateLocalizedText;
  homepageDescription?: AutomationTemplateLocalizedText;
  prompt: AutomationTemplateLocalizedText;
  customize: boolean;
  icon: string;
}

export interface AutomationTemplateCatalog {
  scheduled: ScheduledAutomationTemplate[];
  offPeak: OffPeakAutomationTemplate[];
  rejectedScheduledTemplateIds: string[];
}

/** 云端 Client Scenes 已移除；本地工作流使用 JSON 文档，不再从服务端生成模板。 */
export function mapClientScenesToAutomationTemplates(
  _scenes: readonly unknown[],
  _isValidCronExpr: (cronExpr: string) => boolean,
): AutomationTemplateCatalog {
  return { scheduled: [], offPeak: [], rejectedScheduledTemplateIds: [] };
}

export function resolveAutomationTemplateText(
  text: AutomationTemplateLocalizedText,
  locale?: string,
): string {
  const chinese = locale?.startsWith("zh") ?? false;
  const primary = chinese ? text.cn : text.en;
  const fallback = chinese ? text.en : text.cn;
  return primary?.trim() || fallback?.trim() || "";
}

export function resolveOffPeakTemplateText(
  template: OffPeakAutomationTemplate,
  field: "title" | "description" | "homepageDescription",
  locale: string,
  _formatMessage: (descriptor: { id: string }) => string,
): string {
  const text = field === "homepageDescription"
    ? (template.homepageDescription ?? template.description)
    : template[field];
  return resolveAutomationTemplateText(text, locale);
}

export function materializeScheduledTemplateDraft(
  template: ScheduledAutomationTemplate,
  locale: string,
): { templateId: string; title: string; cronExpr: string; prompt: string } {
  return {
    templateId: template.id,
    title: resolveAutomationTemplateText(template.title, locale),
    cronExpr: template.cronExpr,
    prompt: resolveAutomationTemplateText(template.prompt, locale),
  };
}

export function materializeOffPeakTemplateDraft(
  template: OffPeakAutomationTemplate,
  locale: string,
): { templateId: string; title: string; prompt: string } {
  return {
    templateId: template.id,
    title: resolveAutomationTemplateText(template.title, locale),
    prompt: resolveAutomationTemplateText(template.prompt, locale),
  };
}

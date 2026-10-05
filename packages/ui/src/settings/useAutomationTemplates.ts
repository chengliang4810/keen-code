import { useMemo } from "react";
import type { AutomationTemplateCatalog } from "@/settings/automationTemplateCatalog.js";

export type AutomationTemplateCatalogState = AutomationTemplateCatalog & { loading: boolean };

/**
 * 模板来源固定为本地 JSON 工作流；保留 Hook 形状供原有 Automations 壳复用，
 * 不启动远程请求，也不伪造服务端返回。
 */
export function useAutomationTemplates(_service?: unknown): AutomationTemplateCatalogState {
  return useMemo(
    () => ({ scheduled: [], offPeak: [], rejectedScheduledTemplateIds: [], loading: false }),
    [],
  );
}

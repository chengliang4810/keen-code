import { useEffect, useRef } from "react";
import * as api from "@/lib/api";
import {
  createFrontendResourceSampler,
  observabilityRecordResourceSample,
  type FrontendResourceSampler,
  type FrontendResourceSamplingMode,
} from "@/lib/observability";
import type { AppView } from "@/hooks/useAppRoute";
import type { SettingsSectionId } from "@/lib/settingsCatalog";

export interface FrontendObservabilityOptions {
  appView: AppView;
  settingsSection: SettingsSectionId;
}

/** 维护前端资源采样的唯一生命周期；浏览器 fixture 不调用 Tauri IPC。 */
export function useFrontendObservability({
  appView,
  settingsSection,
}: FrontendObservabilityOptions): void {
  const resourceSamplingContextRef = useRef({ appView, settingsSection });
  resourceSamplingContextRef.current = { appView, settingsSection };
  const resourceSamplerRef = useRef<FrontendResourceSampler | null>(null);

  const getResourceSamplingMode = (): FrontendResourceSamplingMode => {
    if (document.visibilityState === "hidden") return "idle";
    const context = resourceSamplingContextRef.current;
    return context.appView === "settings" && context.settingsSection === "observability"
      ? "panel"
      : "active";
  };

  useEffect(() => {
    if (!api.isTauri()) return;
    const sampler = createFrontendResourceSampler((sample) => {
      void observabilityRecordResourceSample({
        ...sample,
        cpuPercent: null,
        residentBytes: null,
        privateBytes: null,
        virtualBytes: null,
        processCount: null,
      }).catch(() => {});
    }, "idle");
    resourceSamplerRef.current = sampler;
    const updateMode = () => sampler.setMode(getResourceSamplingMode());
    updateMode();
    document.addEventListener("visibilitychange", updateMode);
    return () => {
      document.removeEventListener("visibilitychange", updateMode);
      sampler.stop();
      resourceSamplerRef.current = null;
    };
  }, []);

  useEffect(() => {
    resourceSamplerRef.current?.setMode(getResourceSamplingMode());
  }, [appView, settingsSection]);
}

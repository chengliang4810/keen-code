import type { LocalProviderConfig } from "@/modules/ai/lib/agent";
import type { CustomEndpointKeys } from "@/modules/ai/lib/keyring";

type Settings = LocalProviderConfig & { openaiCompatibleContextLimit?: number };

export function configuredModelSnapshot(
  settings: Settings,
  customEndpointKeys: CustomEndpointKeys = settings.customEndpointKeys ?? {},
  reasoningLevel = settings.reasoningLevel,
): Settings {
  return {
    lmstudioBaseURL: settings.lmstudioBaseURL,
    lmstudioModelId: settings.lmstudioModelId,
    mlxBaseURL: settings.mlxBaseURL,
    mlxModelId: settings.mlxModelId,
    ollamaBaseURL: settings.ollamaBaseURL,
    ollamaModelId: settings.ollamaModelId,
    openaiCompatibleBaseURL: settings.openaiCompatibleBaseURL,
    openaiCompatibleModelId: settings.openaiCompatibleModelId,
    openaiCompatibleContextLimit: settings.openaiCompatibleContextLimit,
    openrouterModelId: settings.openrouterModelId,
    customEndpoints: structuredClone(settings.customEndpoints ?? []),
    customEndpointKeys: { ...customEndpointKeys },
    reasoningLevel,
  };
}

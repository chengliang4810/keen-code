import {
  endpointModels,
  endpointModelSelectionId,
  resolveEndpointModel,
  type CustomEndpoint,
} from "@/modules/ai/config";

export const NO_MODEL_ID = "compat-";

export function configuredCustomModelIds(
  endpoints: readonly CustomEndpoint[],
): string[] {
  return endpoints.flatMap((endpoint) =>
    endpoint.baseURL.trim()
      ? endpointModels(endpoint)
          .filter((model) => model.enabled !== false && model.id.trim())
          .map((model) => endpointModelSelectionId(endpoint, model))
      : [],
  );
}

export function isConfiguredCustomModel(
  id: string,
  endpoints: readonly CustomEndpoint[],
): boolean {
  const resolved = resolveEndpointModel(id, endpoints);
  return (
    !!resolved &&
    !!resolved.endpoint.baseURL.trim() &&
    !!resolved.model.id.trim() &&
    resolved.model.enabled !== false
  );
}

export function selectConfiguredCustomModel(
  preferred: string,
  endpoints: readonly CustomEndpoint[],
): string {
  const resolved = resolveEndpointModel(preferred, endpoints);
  if (resolved && isConfiguredCustomModel(preferred, endpoints))
    return endpointModelSelectionId(resolved.endpoint, resolved.model);
  return configuredCustomModelIds(endpoints)[0] ?? NO_MODEL_ID;
}

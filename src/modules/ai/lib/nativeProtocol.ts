import {
  reasoningRequestBody,
  selectedReasoningLevel,
  type ReasoningBody,
} from "@/modules/ai/lib/reasoning";
import {
  DEFAULT_MODEL_ID,
  endpointIdFromCompatModel,
  effectiveCustomModel,
  resolveEndpointModel,
  getModelContextLimit,
  getProvider,
  isCompatModelId,
  LMSTUDIO_DEFAULT_BASE_URL,
  MLX_DEFAULT_BASE_URL,
  OLLAMA_DEFAULT_BASE_URL,
  resolveModel,
  type MessageProtocol,
} from "@/modules/ai/config";
import type { RunAgentOptions } from "@/modules/ai/lib/agent";

export type NativeModelConfig = {
  protocol: MessageProtocol;
  providerId: string;
  baseUrl: string;
  model: string;
  contextLimit: number;
  secretAccount: string | null;
  allowPrivateNetwork: boolean;
  maxOutputTokens?: number;
  imageInput?: boolean;
  reasoningBody?: ReasoningBody;
};

const CLOUD_URLS: Record<string, string> = {
  openai: "https://api.openai.com/v1",
  anthropic: "https://api.anthropic.com/v1",
  xai: "https://api.x.ai/v1",
  cerebras: "https://api.cerebras.ai/v1",
  groq: "https://api.groq.com/openai/v1",
  deepseek: "https://api.deepseek.com",
  mistral: "https://api.mistral.ai/v1",
  openrouter: "https://openrouter.ai/api/v1",
};

// 协议由供应商或端点配置决定，模型名称不参与猜测。
export function resolveNativeModel(
  opts: RunAgentOptions,
): NativeModelConfig | null {
  const modelId = opts.modelId ?? DEFAULT_MODEL_ID;
  const endpoints = opts.customEndpoints ?? [];
  const info = resolveModel(modelId, endpoints);
  if (info.provider === "google") return null;
  if (isCompatModelId(modelId)) {
    const id = endpointIdFromCompatModel(modelId);
    const resolved = resolveEndpointModel(modelId, endpoints);
    const endpoint = resolved?.endpoint;
    if (
      !endpoint?.baseURL.trim() ||
      !resolved ||
      resolved.model.enabled === false
    ) {
      throw new Error("请先在模型设置中填写服务地址和模型 ID");
    }
    const effective = effectiveCustomModel(resolved.model, endpoint.baseURL);
    return {
      protocol: endpoint.protocol ?? "chat_completions",
      providerId: "openai-compatible",
      baseUrl: endpoint.baseURL.trim(),
      model: resolved.model.id.trim(),
      contextLimit: effective.contextLimit,
      maxOutputTokens: effective.maxOutputTokens,
      imageInput: effective.vision,
      reasoningBody: reasoningRequestBody(
        endpoint.protocol ?? "chat_completions",
        opts.reasoningLevel ??
          selectedReasoningLevel(modelId, effective.reasoningLevels),
        effective.reasoningLevelMap,
        effective.maxOutputTokens,
        resolved.model.id,
      ),
      secretAccount: `compat-${id}-api-key`,
      allowPrivateNetwork: true,
    };
  }
  const localModels: Partial<
    Record<string, [string | undefined, string | undefined]>
  > = {
    lmstudio: [
      opts.lmstudioBaseURL ?? LMSTUDIO_DEFAULT_BASE_URL,
      opts.lmstudioModelId,
    ],
    mlx: [opts.mlxBaseURL ?? MLX_DEFAULT_BASE_URL, opts.mlxModelId],
    ollama: [opts.ollamaBaseURL ?? OLLAMA_DEFAULT_BASE_URL, opts.ollamaModelId],
    "openai-compatible": [
      opts.openaiCompatibleBaseURL,
      opts.openaiCompatibleModelId,
    ],
  };
  const local = localModels[info.provider];
  const model = (
    local
      ? local[1]
      : info.provider === "openrouter" && modelId === "openrouter-custom"
        ? opts.openrouterModelId
        : info.id
  )?.trim();
  const baseUrl = (local ? local[0] : CLOUD_URLS[info.provider])?.trim();
  if (!model || !baseUrl)
    throw new Error("请先在模型设置中填写服务地址和模型 ID");
  return {
    protocol:
      info.provider === "anthropic"
        ? "messages"
        : info.provider === "openai"
          ? "responses"
          : "chat_completions",
    providerId: info.provider,
    baseUrl,
    model,
    contextLimit: getModelContextLimit(
      modelId,
      opts.openaiCompatibleContextLimit,
    ),
    secretAccount: getProvider(info.provider).keyringAccount || null,
    allowPrivateNetwork: !!local,
  };
}

import type { MessageProtocol } from "@/modules/ai/config";
import { EMPTY_PROVIDER_KEYS } from "@/modules/ai/lib/keyring";

export async function testProviderModel({
  modelId,
  baseURL,
  protocol,
  apiKey,
  signal,
}: {
  modelId: string;
  baseURL: string;
  protocol: MessageProtocol;
  apiKey: string | null;
  signal: AbortSignal;
}): Promise<void> {
  const [{ generateText }, { buildLanguageModel }] = await Promise.all([
    import("@/modules/ai/lib/sdkText"),
    import("@/modules/ai/lib/agent"),
  ]);
  signal.throwIfAborted();
  const model = await buildLanguageModel(
    "openai-compatible",
    EMPTY_PROVIDER_KEYS,
    modelId,
    { openaiCompatibleBaseURL: baseURL, protocol },
    apiKey,
  );
  await generateText({
    model,
    prompt: "Reply with OK.",
    maxOutputTokens: 256,
    maxRetries: 0,
    abortSignal: AbortSignal.any([signal, AbortSignal.timeout(30_000)]),
  });
}

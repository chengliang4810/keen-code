import { generateText, readUIMessageStream, stepCountIs } from "ai";
import { DEFAULT_MODEL_ID, type ModelId } from "@/modules/ai/config";
import {
  buildConfiguredLanguageModel,
  type RunAgentOptions,
} from "@/modules/ai/lib/agent";
import { usePreferencesStore } from "@/modules/settings/preferences";
import { useChatStore } from "@/modules/ai/store/chatStore";
import type { ProviderKeys } from "@/modules/ai/lib/keyring";
import type { ToolContext } from "@/modules/ai/tools/context";
import { buildFsTools } from "@/modules/ai/tools/fs";
import { buildSearchTools } from "@/modules/ai/tools/search";
import {
  configuredSubagents,
  resolveSubagentModel,
  subagentSystem,
  type ConfiguredSubagent,
} from "@/modules/ai/agents/config";
import { useSubagentsStore } from "@/modules/ai/store/subagentsStore";
import { resolveNativeModel } from "@/modules/ai/lib/nativeProtocol";
import { currentWorkspaceEnv } from "@/modules/workspace";
import {
  effectiveCustomModel,
  resolveEndpointModel,
  resolveModel,
} from "@/modules/ai/config";

const SUBAGENT_MAX_STEPS = 12;

type Args = {
  type: string;
  definition?: ConfiguredSubagent;
  parentOptions?: RunAgentOptions;
  prompt: string;
  keys: ProviderKeys;
  modelId: string;
  toolContext: ToolContext;
  lmstudioBaseURL?: string;
  onStep?: (label: string) => void;
  abortSignal?: AbortSignal;
};

type RunResult = {
  summary: string;
  stepCount: number;
  durationMs: number;
};

export async function runSubagent({
  type,
  definition,
  parentOptions,
  prompt,
  keys,
  modelId,
  toolContext,
  lmstudioBaseURL,
  onStep,
  abortSignal,
}: Args): Promise<RunResult> {
  const def =
    definition ??
    configuredSubagents(useSubagentsStore.getState().config).find(
      (agent) => agent.id === type,
    );
  if (!def?.enabled)
    throw new Error(`unknown or disabled subagent type: ${type}`);
  const preferences = usePreferencesStore.getState();
  const parent = parentOptions ?? {
    ...preferences,
    keys,
    modelId,
    toolContext,
    uiMessages: [],
    customEndpointKeys: useChatStore.getState().customEndpointKeys,
  };
  const selection = resolveSubagentModel(
    def,
    parent,
    parent.customEndpoints ?? [],
  );
  const childId = `child-${crypto.randomUUID()}`;
  const options: RunAgentOptions = {
    ...parent,
    ...selection,
    keys,
    abortSignal,
    planMode: true,
    permissionMode: "ask",
    onUsage: undefined,
    onFinishMeta: undefined,
    onCompact: undefined,
    toolContext: {
      ...toolContext,
      readCache: new Map(),
      getSessionId: () => childId,
    },
    uiMessages: [
      { id: childId, role: "user", parts: [{ type: "text", text: prompt }] },
    ],
  };
  const start = Date.now();
  const nativeModel = resolveNativeModel(options);
  if (nativeModel && currentWorkspaceEnv().kind === "local") {
    const { runNativeAgentStream } = await import(
      "@/modules/ai/lib/nativeTransport"
    );
    const stream = await runNativeAgentStream(options, nativeModel, def);
    let summary = "";
    let stepCount = 0;
    for await (const message of readUIMessageStream({
      stream,
      terminateOnError: true,
    })) {
      summary = message.parts
        .filter((part) => part.type === "text")
        .map((part) => part.text)
        .join("\n");
      stepCount = message.parts.filter(
        (part) => part.type === "step-start",
      ).length;
    }
    return { summary, stepCount, durationMs: Date.now() - start };
  }

  const readOnly: Record<string, unknown> = {
    ...buildFsTools(options.toolContext),
    ...buildSearchTools(options.toolContext),
  };
  const tools: Record<string, unknown> = {};
  for (const t of def.tools) {
    if (t in readOnly) tools[t] = readOnly[t];
  }

  const selectedId = options.modelId ?? modelId;
  const model = await buildConfiguredLanguageModel(selectedId, keys, {
    ...options,
    lmstudioBaseURL: lmstudioBaseURL ?? options.lmstudioBaseURL,
  });
  const resolved = resolveEndpointModel(
    selectedId,
    options.customEndpoints ?? [],
  );
  const custom = resolved
    ? effectiveCustomModel(resolved.model, resolved.endpoint.baseURL)
    : undefined;
  const result = await generateText({
    model,
    system: subagentSystem(
      def,
      parent.globalInstructions,
      parent.projectInstructions,
    ),
    maxOutputTokens: custom?.maxOutputTokens,
    providerOptions:
      resolveModel(selectedId, options.customEndpoints ?? []).provider ===
        "google" && options.reasoningLevel
        ? {
            google: {
              thinkingConfig: { thinkingLevel: options.reasoningLevel },
            },
          }
        : undefined,
    prompt,
    tools: tools as Parameters<typeof generateText>[0]["tools"],
    stopWhen: stepCountIs(SUBAGENT_MAX_STEPS),
    abortSignal,
    onStepFinish: (step) => {
      if (!onStep) return;
      const last = step.toolCalls?.[step.toolCalls.length - 1];
      if (last) onStep(`${type}: ${last.toolName}`);
    },
  });

  return {
    summary: result.text || "(no output)",
    stepCount: result.steps?.length ?? 0,
    durationMs: Date.now() - start,
  };
}

export const DEFAULT_SUBAGENT_MODEL: ModelId = DEFAULT_MODEL_ID;

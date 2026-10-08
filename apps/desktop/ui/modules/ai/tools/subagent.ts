import { tool } from "ai";
import { z } from "zod";
import { runSubagent } from "@/modules/ai/agents/runSubagent";
import { configuredSubagents } from "@/modules/ai/agents/config";
import { useSubagentsStore } from "@/modules/ai/store/subagentsStore";
import type { RunAgentOptions } from "@/modules/ai/lib/agent";
import { useChatStore } from "@/modules/ai/store/chatStore";
import type { ToolContext } from "@/modules/ai/tools/context";

export function buildSubagentTools(
  ctx: ToolContext,
  parentOptions?: RunAgentOptions,
) {
  const agents = configuredSubagents(
    useSubagentsStore.getState().config,
  ).filter((agent) => agent.enabled);
  const typeKeys = agents.map((agent) => agent.id) as [string, ...string[]];
  return {
    run_subagent: tool({
      description: `Spawn an isolated subagent with its own restricted toolset and a fresh message history. Use when you need to delegate a self-contained read-only investigation (large search, code review, security audit) without polluting your own context. The subagent returns a single text summary; pick a 'type' that matches its job.

Types:
${agents.map((agent) => `- ${agent.id}: ${agent.description}`).join("\n")}

Auto-executes (no approval) — subagents are read-only by design.`,
      inputSchema: z.object({
        type: z.enum(typeKeys),
        prompt: z
          .string()
          .describe(
            "Self-contained instruction. The subagent has no memory of prior conversation — include all relevant context.",
          ),
        description: z
          .string()
          .optional()
          .describe("Short label shown in the chat UI for the spawn card."),
      }),
      execute: async ({ type, prompt, description }, { abortSignal }) => {
        const { apiKeys, selectedModelId, patchAgentMeta } =
          useChatStore.getState();
        try {
          const r = await runSubagent({
            type,
            definition: agents.find((agent) => agent.id === type),
            parentOptions,
            prompt,
            keys: parentOptions?.keys ?? apiKeys,
            modelId: parentOptions?.modelId ?? selectedModelId,
            toolContext: ctx,
            onStep: (label) => patchAgentMeta({ step: label }),
            abortSignal,
          });
          return {
            type,
            description,
            summary: r.summary,
            stepCount: r.stepCount,
            durationMs: r.durationMs,
          };
        } catch (e) {
          return { error: String(e), type };
        }
      },
    }),
  } as const;
}

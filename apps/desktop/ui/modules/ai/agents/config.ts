import { z } from "zod";
import {
  SUBAGENTS,
  READ_ONLY_TOOLS,
  type SubagentDef,
} from "@/modules/ai/agents/registry";
import {
  effectiveCustomModel,
  resolveEndpointModel,
  type CustomEndpoint,
} from "@/modules/ai/config";
import { isConfiguredCustomModel } from "@/modules/ai/lib/modelSelection";

export const SUBAGENT_COLORS = [
  "blue",
  "green",
  "yellow",
  "red",
  "purple",
  "orange",
  "pink",
  "gray",
] as const;
const modelFields = {
  modelId: z.string().trim().min(1).max(512).optional(),
  reasoningLevel: z.string().trim().min(1).max(64).optional(),
};
const profileSchema = z
  .object({
    id: z.string().regex(/^custom-[a-z0-9-]{1,64}$/),
    label: z.string().trim().min(1).max(128),
    description: z.string().trim().min(1).max(1024),
    systemPrompt: z.string().trim().min(1).max(65536),
    tools: z
      .array(z.enum(READ_ONLY_TOOLS as [string, ...string[]]))
      .max(4)
      .refine((tools) => new Set(tools).size === tools.length),
    enabled: z.boolean(),
    color: z.enum(SUBAGENT_COLORS),
    injectAgentsMd: z.boolean(),
    ...modelFields,
  })
  .strict();
const overrideSchema = z
  .object({
    id: z.enum(["explore", "code-review", "security", "general"]),
    ...modelFields,
  })
  .strict();
const configSchema = z
  .object({
    version: z.literal(1),
    custom: z.array(profileSchema).max(128),
    overrides: z.array(overrideSchema).max(4),
  })
  .strict()
  .superRefine((config, ctx) => {
    for (const entries of [config.custom, config.overrides]) {
      if (new Set(entries.map((entry) => entry.id)).size !== entries.length)
        ctx.addIssue({ code: "custom", message: "Already in use." });
    }
    if (
      new Set(config.custom.map((entry) => entry.label.toLowerCase())).size !==
      config.custom.length
    )
      ctx.addIssue({ code: "custom", message: "Already in use." });
    for (const entry of [...config.custom, ...config.overrides]) {
      if (entry.reasoningLevel && !entry.modelId)
        ctx.addIssue({ code: "custom", message: "Select a model" });
    }
  });

export type SubagentProfile = z.infer<typeof profileSchema>;
export type SubagentConfig = z.infer<typeof configSchema>;
export type ConfiguredSubagent = SubagentDef & {
  builtIn: boolean;
  enabled: boolean;
  color: (typeof SUBAGENT_COLORS)[number];
  injectAgentsMd: boolean;
  modelId?: string;
  reasoningLevel?: string;
};
export const EMPTY_SUBAGENT_CONFIG: SubagentConfig = {
  version: 1,
  custom: [],
  overrides: [],
};

export function parseSubagentConfig(value: unknown): SubagentConfig {
  return configSchema.parse(
    value === undefined ? EMPTY_SUBAGENT_CONFIG : value,
  );
}

export function configuredSubagents(
  config: SubagentConfig,
): ConfiguredSubagent[] {
  return [
    ...Object.values(SUBAGENTS).map(
      (agent): ConfiguredSubagent => ({
        ...agent,
        builtIn: true,
        enabled: true,
        color: "gray",
        injectAgentsMd: false,
        ...config.overrides.find((override) => override.id === agent.id),
      }),
    ),
    ...config.custom.map((agent) => ({ ...agent, builtIn: false })),
  ];
}

export function resolveSubagentModel(
  agent: Pick<ConfiguredSubagent, "modelId" | "reasoningLevel">,
  parent: { modelId?: string; reasoningLevel?: string },
  endpoints: readonly CustomEndpoint[],
): { modelId?: string; reasoningLevel?: string } {
  if (!agent.modelId)
    return { modelId: parent.modelId, reasoningLevel: parent.reasoningLevel };
  if (!isConfiguredCustomModel(agent.modelId, endpoints))
    throw new Error("Select a model");
  const resolved = resolveEndpointModel(agent.modelId, endpoints);
  if (!resolved) throw new Error("Select a model");
  const levels = effectiveCustomModel(
    resolved.model,
    resolved.endpoint.baseURL,
  ).reasoningLevels;
  if (agent.reasoningLevel && !levels.includes(agent.reasoningLevel))
    throw new Error("Select a valid reasoning level before sending.");
  return {
    modelId: agent.modelId,
    reasoningLevel: agent.reasoningLevel ?? levels[levels.length - 1],
  };
}

export function subagentSystem(
  agent: ConfiguredSubagent,
  global?: string,
  project?: string | null,
): string {
  return [
    agent.systemPrompt,
    ...(agent.injectAgentsMd ? [global, project] : []),
  ]
    .filter(Boolean)
    .join("\n\n");
}

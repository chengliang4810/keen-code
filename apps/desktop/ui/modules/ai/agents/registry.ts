import templates from "../../../../../../crates/rcode-runtime/resources/subagents.json";

export type BuiltinSubagentType =
  | "explore"
  | "code-review"
  | "security"
  | "general";
export type SubagentType = string;

export type SubagentDef = {
  id: SubagentType;
  label: string;
  description: string;
  /**
   * Whitelist of tools the subagent may call. Excludes mutating tools and
   * `run_subagent` itself to prevent recursion. The runner filters down the
   * main toolset to this list before constructing the inner Agent.
   */
  tools: string[];
  systemPrompt: string;
};

export const READ_ONLY_TOOLS = ["read_file", "list_directory", "grep", "glob"];

export const SUBAGENTS = Object.fromEntries(
  templates.map((template) => [template.id, template]),
) as Record<BuiltinSubagentType, SubagentDef>;

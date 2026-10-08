import { invoke } from "@tauri-apps/api/core";
import { CommandLineIcon } from "@hugeicons/core-free-icons";
import type { AgentResource } from "@/modules/ai/lib/extensions";
import {
  SLASH_COMMANDS,
  type SlashCommandMeta,
} from "@/modules/ai/lib/slashCommands";

export type CommandScope = "user" | "project";
export type CommandConfig = {
  name: string;
  description: string;
  argumentHint: string;
  prompt: string;
  enabled: boolean;
};
export type CommandEntry = Omit<CommandConfig, "prompt"> & {
  scope: CommandScope;
  path: string;
};
export type CommandFile = CommandConfig & { content: string };
export type PluginCommand = {
  name: string;
  description: string;
  path: string;
  pluginName: string;
  marketplace: string | null;
};
export type CommandCatalog = {
  commands: CommandEntry[];
  pluginCommands: PluginCommand[];
  diagnostics: string[];
};
export type ImportSelection = {
  agent: string;
  scope: CommandScope;
  relativePath: string;
  expectedContent: string;
};
export type ExternalCommand = {
  name: string;
  description: string;
  agentLabel: string;
  path: string;
  selection: ImportSelection;
};
export type ImportCatalog = {
  commands: ExternalCommand[];
  diagnostics: string[];
};
export type ImportResult = {
  id: string;
  name: string;
  status: "imported" | "skipped" | "failed";
  error: string | null;
};

export const commandApi = {
  list: (cwd: string | null) =>
    invoke<CommandCatalog>("agent_commands_list", { cwd }),
  read: (name: string) => invoke<CommandFile>("agent_commands_read", { name }),
  save: (config: CommandConfig, expectedContent: string | null) =>
    invoke<CommandFile>("agent_commands_save", {
      config,
      expectedContent,
    }),
  remove: (name: string, expectedContent: string) =>
    invoke<void>("agent_commands_delete", {
      name,
      expectedContent,
    }),
  resolve: (cwd: string | null, name: string, argumentsText: string) =>
    invoke<string | null>("agent_commands_resolve", {
      cwd,
      name,
      arguments: argumentsText,
    }),
  discover: () => invoke<ImportCatalog>("agent_commands_discover"),
  import: (selections: ImportSelection[]) =>
    invoke<ImportResult[]>("agent_commands_import", { selections }),
};

const BUILTIN_NAMES = new Set(Object.keys(SLASH_COMMANDS));

export function isValidCommandName(name: string): boolean {
  return (
    /^[a-zA-Z0-9_-]{1,50}$/.test(name) &&
    !/^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/i.test(name) &&
    !BUILTIN_NAMES.has(name.toLowerCase())
  );
}

export function parseCommandInvocation(
  value: string,
): { name: string; arguments: string } | null {
  const match = /^\/([a-zA-Z0-9_-]{1,50})(?:\s+([\s\S]*))?$/.exec(value.trim());
  return match ? { name: match[1], arguments: match[2] ?? "" } : null;
}

export function availableCommandResources(
  resources: readonly AgentResource[],
): AgentResource[] {
  const byName = new Map<string, AgentResource>();
  for (const entry of resources) {
    const key = entry.name.toLowerCase();
    if (entry.source === "project" || byName.get(key)?.source !== "project")
      byName.set(key, entry);
  }
  return [...byName.values()].filter((entry) => entry.enabled);
}

export function resourceCommand(entry: AgentResource): SlashCommandMeta {
  return {
    name: entry.name,
    invocation: `/${entry.name}`,
    label: entry.description || entry.name,
    icon: CommandLineIcon,
    source: entry.source === "plugin" ? "plugin" : "custom",
  };
}

export function importKey(command: ExternalCommand): string {
  return JSON.stringify([
    command.selection.agent,
    command.selection.scope,
    command.selection.relativePath,
  ]);
}

import { invoke } from "@tauri-apps/api/core";

export type McpServerEntry = {
  id: string;
  enabled: boolean;
  transport: "stdio" | "http";
  command: string;
  args: string[];
  url: string;
};
export type ExtensionConfig = { mcpServers: McpServerEntry[] };
export type InstalledPlugin = {
  id: { plugin: string; marketplace: string | null };
  version: string;
  installPath: string;
  enabled: boolean;
  publicUserConfig: Record<string, unknown>;
  sensitiveUserConfigKeys: string[];
  source?:
    | { kind: "local"; path: string }
    | { kind: "marketplace"; source: string }
    | null;
};
export type PluginConfigField = {
  type: "string" | "number" | "boolean" | "select" | "directory" | "file";
  title: string | null;
  default: unknown;
  required: boolean;
  sensitive: boolean;
  multiple: boolean;
  min: number | null;
  max: number | null;
  enum: unknown[];
};
export type PluginDetails = {
  plugin: InstalledPlugin;
  description: string | null;
  fields: Record<string, PluginConfigField>;
  components: Record<string, number>;
};
export type MarketplaceSource = { name: string; source: string };
export type AvailablePlugin = {
  name: string;
  marketplace: string;
  description: string | null;
  version: string | null;
  category: string | null;
  keywords: string[];
  installed: boolean;
};
export type MarketplaceCatalog = {
  initialized: boolean;
  sources: MarketplaceSource[];
  plugins: AvailablePlugin[];
};
export type AgentResource = {
  name: string;
  description: string;
  source: "project" | "user" | "plugin";
  path: string | null;
  enabled: boolean;
};
export type AgentResources = {
  skills: AgentResource[];
  commands: AgentResource[];
  hooks: AgentResource[];
  diagnostics: string[];
  truncated: boolean;
};
export const extensionApi = {
  resources: (cwd: string) =>
    invoke<AgentResources>("agent_resources_list", { cwd }),
  userResources: () =>
    invoke<AgentResources>("agent_resources_list", { cwd: null }),
  get: () => invoke<ExtensionConfig>("agent_extensions_get"),
  save: (config: ExtensionConfig) =>
    invoke<void>("agent_extensions_save", { config }),
  credentials: (
    serverId: string,
    token: string | null,
    environment: Record<string, string> | null,
  ) => invoke<void>("agent_mcp_set_secrets", { serverId, token, environment }),
  plugins: () => invoke<{ plugins: InstalledPlugin[] }>("agent_plugins_list"),
  install: (
    sourceRoot: string,
    pluginId: string,
    values: Record<string, unknown>,
  ) => invoke<void>("agent_plugins_install", { sourceRoot, pluginId, values }),
  enable: (pluginId: string, enabled: boolean) =>
    invoke<void>("agent_plugins_enable", { pluginId, enabled }),
  uninstall: (pluginId: string) =>
    invoke<void>("agent_plugins_uninstall", { pluginId }),
  configure: (pluginId: string, values: Record<string, unknown>) =>
    invoke<void>("agent_plugins_configure", { pluginId, values }),
  details: (pluginId: string) =>
    invoke<PluginDetails>("agent_plugins_details", { pluginId }),
  pickPath: (directory: boolean, title: string) =>
    invoke<string | null>("agent_plugins_pick_path", { directory, title }),
  update: (pluginId: string | null = null) =>
    invoke<void>("agent_plugins_update", { pluginId }),
  marketplace: () => invoke<MarketplaceCatalog>("agent_marketplace_catalog"),
  initializeMarketplace: () =>
    invoke<string[]>("agent_marketplace_refresh", {
      name: null,
      restoreDefault: true,
      initializeOnly: true,
    }),
  addMarketplace: (source: string) =>
    invoke<void>("agent_marketplace_add", { source }),
  removeMarketplace: (name: string) =>
    invoke<void>("agent_marketplace_remove", { name }),
  refreshMarketplace: (name: string | null = null, restoreDefault = false) =>
    invoke<string[]>("agent_marketplace_refresh", { name, restoreDefault }),
  installMarketplacePlugin: (pluginId: string) =>
    invoke<void>("agent_marketplace_install", { pluginId }),
};

export function parseExtensionObject(text: string): Record<string, unknown> {
  const value: unknown = JSON.parse(text);
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new Error("JSON must be an object");
  return value as Record<string, unknown>;
}

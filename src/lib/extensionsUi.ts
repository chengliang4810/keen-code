/** 设置 → 扩展（Skills / MCP / Plugins）的纯前端辅助函数。 */

import type { PluginDto, SkillDto, SkillSource } from "./api";

/** 返回当前唯一 Skill 来源对应的徽标色调。 */
export function skillSourceTone(
  source: SkillSource,
): "user" | "project" | "plugin" {
  return source;
}

/** 生成 Skill 名称下方的来源与可调用摘要。 */
export function skillMetaLine(
  skill: Pick<SkillDto, "source" | "userInvocable">,
): string {
  const parts: string[] = [skill.source];
  if (skill.userInvocable) parts.push("user-invocable");
  return parts.join(" · ");
}

/** 按名称字母序返回 Skill 列表的稳定副本。 */
export function sortSkillsByName<T extends { name: string }>(skills: T[]): T[] {
  return [...skills].sort((a, b) =>
    a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
  );
}

/** 按名称字母序返回 MCP Server 列表的稳定副本。 */
export function sortMcpByName<T extends { name: string }>(servers: T[]): T[] {
  return [...servers].sort((a, b) =>
    a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
  );
}

/** 缩短次要界面中的长路径，保留父目录与文件名。 */
export function shortPathLabel(
  path: string | null,
  max = 56,
): string {
  const p = (path ?? "").trim();
  if (!p) return "";
  if (p.length <= max) return p;
  const sep = p.includes("\\") && !p.includes("/") ? "\\" : "/";
  const parts = p.split(/[/\\]/).filter(Boolean);
  if (parts.length <= 2) return `…${sep}${parts.join(sep)}`;
  const tail = parts.slice(-2).join(sep);
  const candidate = `…${sep}${tail}`;
  return candidate.length <= max ? candidate : `…${sep}${parts[parts.length - 1]}`;
}

/**
 * 合并 Skills、MCP 与插件的当前错误，去除重复文本。
 */
export function mergeInspectErrors(
  skillsError: string | null,
  mcpError: string | null,
  pluginsError: string | null,
): string | null {
  const parts = [skillsError, mcpError, pluginsError]
    .map((x) => (x ?? "").trim())
    .filter(Boolean);
  if (parts.length === 0) return null;
  const unique = [...new Set(parts)];
  return unique.join(" · ");
}

/** 按名称字母序返回插件列表的稳定副本。 */
export function sortPluginsByName<T extends { name: string }>(plugins: T[]): T[] {
  return [...plugins].sort((a, b) =>
    a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
  );
}

/** 返回插件当前启用状态对应的徽标色调。 */
export function pluginStatusTone(
  enabled: boolean,
): "enabled" | "disabled" {
  return enabled ? "enabled" : "disabled";
}

/** 插件组件摘要的运行时边界输入。 */
type PluginProvidesInput = {
  /** 后端当前必须返回组件信息；null 用于检测契约违规。 */
  provides: PluginDto["provides"] | null;
};

/**
 * 插件提供的 Skill 数量摘要。
 */
export function pluginProvidesLine(plugin: PluginProvidesInput): string {
  if (!plugin.provides) {
    throw new Error("插件 provides 缺失");
  }
  const counts: string[] = [];
  const skills = plugin.provides.skills ?? 0;
  if (skills > 0) counts.push(`${skills} skill${skills === 1 ? "" : "s"}`);
  const commands = plugin.provides.commands ?? 0;
  if (commands > 0) counts.push(`${commands} command${commands === 1 ? "" : "s"}`);
  const agents = plugin.provides.agents ?? 0;
  if (agents > 0) counts.push(`${agents} agent${agents === 1 ? "" : "s"}`);
  const hooks = plugin.provides.hooks ?? 0;
  if (hooks > 0) counts.push(`${hooks} hook${hooks === 1 ? "" : "s"}`);
  const mcp = plugin.provides.mcp ?? 0;
  if (mcp > 0) counts.push(`${mcp} MCP`);
  return counts.join(" · ");
}

/**
 * 插件 hooks.json 中声明了但 peri 无法识别的事件名摘要；无未识别事件时返回 null。
 */
export function pluginUnsupportedHooksLine(plugin: {
  unsupportedHooks?: string[];
}): string | null {
  const unsupportedHooks = plugin.unsupportedHooks ?? [];
  if (unsupportedHooks.length === 0) return null;
  const count = unsupportedHooks.length;
  return `${count} unsupported hook event${count === 1 ? "" : "s"}: ${unsupportedHooks.join(", ")}`;
}

/** 返回插件行的唯一列表键；当前合约保证插件名称唯一。 */
export function pluginRowKey(plugin: Pick<PluginDto, "name">): string {
  return plugin.name;
}

export type PluginFilter = "all" | "enabled" | "disabled";

/** 按当前启用状态筛选插件。 */
export function filterPluginsByLoadState<T extends { enabled: boolean }>(
  plugins: T[],
  filter: PluginFilter,
): T[] {
  if (filter === "enabled") return plugins.filter((p) => p.enabled);
  if (filter === "disabled") return plugins.filter((p) => !p.enabled);
  return plugins;
}

/**
 * 规范化插件安装来源（本地路径或本地市场中的 name[@marketplace]）。
 * 空白输入返回 null。
 */
export function normalizePluginInstallSource(
  raw: string,
): string | null {
  const s = raw.trim();
  return s ? s : null;
}

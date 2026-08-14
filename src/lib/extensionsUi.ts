/** 设置 → 扩展（Skills / MCP / Plugins）的纯前端辅助函数。 */

import type {
  McpDto,
  McpOAuthStatus,
  McpRuntimeInitPhase,
  McpRuntimeSnapshot,
  McpRuntimeStatus,
  PluginDto,
  SkillDto,
  SkillSource,
} from "./api";
import type { AcpEvent } from "./acp/events";

/** 合并静态配置与 Peri 运行态后的 MCP 界面投影。 */
export interface McpServerView {
  /** MCP Server 稳定名称。 */
  name: string;
  /** KeenCode 静态配置；仅存在于运行态的残留 Server 为 null。 */
  config: McpDto | null;
  /** 当前静态配置是否启用；运行态残留 Server 按其运行状态推断。 */
  enabled: boolean;
  /** 静态配置中的命令或 URL。 */
  target: string | null;
  /** 优先使用 Peri 实际运行态的传输类型。 */
  transport: string;
  /** 当前连接状态；尚无运行态记录时由静态启用状态推导。 */
  runtimeStatus: McpRuntimeStatus;
  /** Peri 已发现的工具数量。 */
  toolsCount: number;
  /** 当前 OAuth 授权状态。 */
  oauthStatus: McpOAuthStatus;
  /** 当前运行失败原因。 */
  error: string | null;
}

/** MCP 运行状态或初始化阶段使用的界面色调。 */
export type McpRuntimeTone = "ok" | "fail" | "muted";

/** Host 级 MCP OAuth 事件投影出的唯一界面动作。 */
export type McpOAuthUiAction =
  | {
      /** 打开系统浏览器继续授权。 */
      type: "open_authorization";
      /** 发起授权的 MCP Server 名称。 */
      serverName: string;
      /** Peri 返回的 OAuth 授权地址。 */
      authorizationUrl: string;
    }
  | {
      /** 刷新 MCP 运行态。 */
      type: "refresh";
      /** 状态发生变化的 MCP Server 名称。 */
      serverName: string;
      /** OAuth 失败原因；成功或凭据恢复时为空。 */
      error: string | null;
    };

/** 手动 OAuth 回调输入的解析错误。 */
export type McpOAuthCallbackParseError =
  | "empty"
  | "missing_code"
  | "missing_state"
  | "state_mismatch";

/** 手动 OAuth 回调输入的解析结果。 */
export type McpOAuthCallbackParseResult =
  | {
      /** 输入包含完整且可信的 code 与 state。 */
      ok: true;
      /** OAuth 授权码。 */
      code: string;
      /** OAuth CSRF 校验状态。 */
      state: string;
    }
  | {
      /** 输入无法安全提交。 */
      ok: false;
      /** 供界面本地化展示的稳定错误码。 */
      error: McpOAuthCallbackParseError;
    };

/**
 * 合并 KeenCode 静态 MCP 配置与 Peri 运行态，并按名称稳定排序。
 * 静态配置决定可编辑字段，运行态只覆盖连接信息。
 */
export function mergeMcpServers(
  configuredServers: McpDto[],
  runtimeSnapshot: McpRuntimeSnapshot | null,
): McpServerView[] {
  const runtimeByName = new Map(
    (runtimeSnapshot?.servers ?? []).map((server) => [server.name, server]),
  );
  const rows = configuredServers.map<McpServerView>((config) => {
    const runtime = runtimeByName.get(config.name);
    runtimeByName.delete(config.name);
    return {
      name: config.name,
      config,
      enabled: config.enabled,
      target: config.target,
      transport: runtime?.transport.trim() || config.transport,
      runtimeStatus:
        runtime?.status ?? (config.enabled ? "uninitialized" : "disabled"),
      toolsCount: runtime?.toolsCount ?? 0,
      oauthStatus: runtime?.oauthStatus ?? "none",
      error: runtime?.error ?? null,
    };
  });

  for (const runtime of runtimeByName.values()) {
    rows.push({
      name: runtime.name,
      config: null,
      enabled: runtime.status !== "disabled",
      target: null,
      transport: runtime.transport.trim() || "unknown",
      runtimeStatus: runtime.status,
      toolsCount: runtime.toolsCount,
      oauthStatus: runtime.oauthStatus,
      error: runtime.error,
    });
  }

  return sortMcpByName(rows);
}

/** 返回单个 MCP 连接状态对应的界面色调。 */
export function mcpRuntimeStatusTone(
  status: McpRuntimeStatus,
): McpRuntimeTone {
  if (status === "connected") return "ok";
  if (status === "failed") return "fail";
  return "muted";
}

/** 返回 MCP 连接池初始化阶段对应的界面色调。 */
export function mcpRuntimePhaseTone(
  phase: McpRuntimeInitPhase,
): McpRuntimeTone {
  if (phase === "ready") return "ok";
  if (phase === "failed") return "fail";
  return "muted";
}

/** 将 Host 级 Peri OAuth 事件收敛为浏览器打开或状态刷新动作。 */
export function projectMcpOAuthUiAction(
  event: AcpEvent,
): McpOAuthUiAction | null {
  switch (event.type) {
    case "oauth_needed":
      return {
        type: "open_authorization",
        serverName: event.value.server_name,
        authorizationUrl: event.value.auth_url,
      };
    case "oauth_failed":
      return {
        type: "refresh",
        serverName: event.value.server_name,
        error: event.value.error,
      };
    case "oauth_completed":
    case "oauth_restored":
      return {
        type: "refresh",
        serverName: event.value.server_name,
        error: null,
      };
    default:
      return null;
  }
}

/** 从 OAuth 授权地址中读取 Peri 生成的预期 state。 */
function expectedMcpOAuthState(authorizationUrl: string | null): string | null {
  const value = authorizationUrl?.trim();
  if (!value) return null;
  try {
    return new URL(value).searchParams.get("state")?.trim() || null;
  } catch {
    return null;
  }
}

/**
 * 解析用户粘贴的 OAuth 回调 URL、查询串或授权码。
 * 仅输入授权码时使用授权地址中的 state，并拒绝与预期 state 不一致的回调。
 */
export function parseMcpOAuthCallbackInput(
  input: string,
  authorizationUrl: string | null,
): McpOAuthCallbackParseResult {
  const value = input.trim();
  if (!value) return { ok: false, error: "empty" };

  const expectedState = expectedMcpOAuthState(authorizationUrl);
  let code: string | null = null;
  let state: string | null = null;
  const containsCallbackParams =
    value.startsWith("?") ||
    value.startsWith("#") ||
    value.includes("code=") ||
    value.includes("state=");

  if (containsCallbackParams) {
    try {
      const parsed = value.includes("://")
        ? new URL(value)
        : new URL(value.startsWith("?") ? value : `?${value.replace(/^#/, "")}`, "http://localhost");
      const params = parsed.searchParams.size > 0
        ? parsed.searchParams
        : new URLSearchParams(parsed.hash.replace(/^#/, ""));
      code = params.get("code")?.trim() || null;
      state = params.get("state")?.trim() || null;
    } catch {
      return { ok: false, error: "missing_code" };
    }
  } else {
    code = value;
    state = expectedState;
  }

  if (!code) return { ok: false, error: "missing_code" };
  if (!state) return { ok: false, error: "missing_state" };
  if (expectedState && state !== expectedState) {
    return { ok: false, error: "state_mismatch" };
  }
  return { ok: true, code, state };
}

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
  const lsp = plugin.provides.lsp ?? 0;
  if (lsp > 0) counts.push(`${lsp} LSP`);
  return counts.join(" · ");
}

/** 插件包含 LSP Server 时，其启停和配置变更必须提示重启后生效。 */
export function pluginLspRequiresRestart(plugin: PluginProvidesInput): boolean {
  if (!plugin.provides) {
    throw new Error("插件 provides 缺失");
  }
  return (plugin.provides.lsp ?? 0) > 0;
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

/** 设置 → 扩展：管理 Skills、MCP 与 KeenCode 本地插件。 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import { GlassModal } from "@/components/GlassModal";
import {
  IconDoctor,
  IconFolder,
  IconPlus,
  IconPlug,
  IconPuzzle,
  IconRefresh,
  IconSkills,
  IconTrash,
} from "@/components/icons";
import {
  filterPluginsByLoadState,
  mcpRuntimePhaseTone,
  mcpRuntimeStatusTone,
  mergeMcpServers,
  mergeInspectErrors,
  normalizePluginInstallSource,
  parseMcpOAuthCallbackInput,
  pluginLspRequiresRestart,
  pluginProvidesLine,
  pluginRowKey,
  pluginUnsupportedHooksLine,
  pluginStatusTone,
  projectMcpOAuthUiAction,
  shortPathLabel,
  skillMetaLine,
  skillSourceTone,
  sortMcpByName,
  sortPluginsByName,
  sortSkillsByName,
  type McpServerView,
  type McpOAuthCallbackParseError,
  type PluginFilter,
} from "@/lib/extensionsUi";
import { ExtensionsBuildExtras } from "@/components/ExtensionsBuildExtras";
import { listenAcp } from "@/lib/acp/api";
import { parseAgentEvent } from "@/lib/acp/events";

export type ExtensionsTabId = "market" | "skills" | "mcp";

export interface ExtensionsPanelProps {
  locale: Locale;
  /** 当前工作台项目路径，仅用于项目级 Skills。 */
  projectPath?: string | null;
  /** Page tab from settings hash (`#/settings/extensions/{tab}`). */
  activeTab?: ExtensionsTabId;
  /** 切换扩展页签。 */
  onTabChange?: (tab: ExtensionsTabId) => void;
}

/** MCP OAuth 前端长流程的当前阶段。 */
type McpOauthFlowPhase =
  | "starting"
  | "awaiting_callback"
  | "submitting"
  | "canceling";

/** 当前 MCP OAuth 长流程。 */
interface McpOauthFlowState {
  /** MCP Server 稳定名称。 */
  serverName: string;
  /** 启动、等待回调、提交或取消阶段。 */
  phase: McpOauthFlowPhase;
  /** Peri 生成的授权地址，用于重新打开页面和校验 state。 */
  authorizationUrl: string | null;
  /** 用户粘贴的回调 URL、查询串或授权码。 */
  callbackInput: string;
}

/** MCP 运行态摘要组件属性。 */
export interface McpRuntimeDetailsProps {
  /** 当前界面语言。 */
  locale: Locale;
  /** 已合并静态配置与运行态的 MCP Server。 */
  server: McpServerView;
  /** 优先展示的 OAuth 或系统浏览器错误。 */
  error?: string | null;
}

/** 返回 MCP 连接状态的本地化文案。 */
function mcpRuntimeStatusLabel(
  tr: ReturnType<typeof createT>,
  status: api.McpRuntimeStatus,
): string {
  switch (status) {
    case "connected":
      return tr("ext.mcp.status.connected");
    case "failed":
      return tr("ext.mcp.status.failed");
    case "disconnected":
      return tr("ext.mcp.status.disconnected");
    case "disabled":
      return tr("ext.mcp.status.disabled");
    case "uninitialized":
      return tr("ext.mcp.status.uninitialized");
  }
}

/** 返回 MCP 连接池初始化阶段的本地化文案。 */
function mcpRuntimePhaseLabel(
  tr: ReturnType<typeof createT>,
  phase: api.McpRuntimeInitPhase,
): string {
  switch (phase) {
    case "pending":
      return tr("ext.mcp.runtime.pending");
    case "initializing":
      return tr("ext.mcp.runtime.initializing");
    case "ready":
      return tr("ext.mcp.runtime.ready");
    case "failed":
      return tr("ext.mcp.runtime.failed");
  }
}

/** 返回手动 OAuth 回调解析错误的本地化文案。 */
function mcpOAuthCallbackErrorLabel(
  tr: ReturnType<typeof createT>,
  error: McpOAuthCallbackParseError,
): string {
  switch (error) {
    case "empty":
      return tr("ext.mcp.oauthCallback.error.empty");
    case "missing_code":
      return tr("ext.mcp.oauthCallback.error.missingCode");
    case "missing_state":
      return tr("ext.mcp.oauthCallback.error.missingState");
    case "state_mismatch":
      return tr("ext.mcp.oauthCallback.error.stateMismatch");
  }
}

/** 展示单个 MCP Server 的连接、传输、工具数、OAuth 与错误信息。 */
export function McpRuntimeDetails({
  locale,
  server,
  error = null,
}: McpRuntimeDetailsProps) {
  const tr = createT(locale);
  const tone = mcpRuntimeStatusTone(server.runtimeStatus);
  return (
    <div
      className="ext-item__meta ext-mcp-runtime"
      data-mcp-status={server.runtimeStatus}
    >
      <span className={`ext-badge ext-badge--${tone}`}>
        {mcpRuntimeStatusLabel(tr, server.runtimeStatus)}
      </span>
      <span className="ext-badge ext-badge--muted">
        {tr("ext.mcp.transport", { transport: server.transport })}
      </span>
      <span>{tr("ext.mcp.toolsCount", { count: server.toolsCount })}</span>
      {server.oauthStatus === "authorized" ? (
        <span className="ext-badge ext-badge--ok">
          {tr("ext.mcp.oauth.authorized")}
        </span>
      ) : null}
      {server.oauthStatus === "needs_authorization" ? (
        <span className="ext-badge ext-badge--fail">
          {tr("ext.mcp.oauth.needsAuthorization")}
        </span>
      ) : null}
      {error ? (
        <span className="ext-mcp-runtime__error" role="alert">
          {error}
        </span>
      ) : null}
    </div>
  );
}

export function ExtensionsPanel({
  locale,
  projectPath = null,
  activeTab = "market",
  onTabChange,
}: ExtensionsPanelProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [skills, setSkills] = useState<api.SkillDto[]>([]);
  const [servers, setServers] = useState<api.McpDto[]>([]);
  /** Peri 连接池当前只读快照。 */
  const [mcpRuntime, setMcpRuntime] =
    useState<api.McpRuntimeSnapshot | null>(null);
  const [plugins, setPlugins] = useState<api.PluginDto[]>([]);
  const [skillsError, setSkillsError] = useState<string | null>(null);
  const [mcpError, setMcpError] = useState<string | null>(null);
  /** MCP 运行态查询或事件订阅错误。 */
  const [mcpRuntimeError, setMcpRuntimeError] = useState<string | null>(null);
  /** OAuth 事件产生的逐 Server 错误。 */
  const [mcpOauthErrors, setMcpOauthErrors] = useState<Record<string, string>>(
    {},
  );
  /** 当前 OAuth 长流程；终态事件到达前保持占用，阻止重复启动。 */
  const [mcpOauthFlow, setMcpOauthFlow] =
    useState<McpOauthFlowState | null>(null);
  /** OAuth 长流程同步锁，避免 React 重渲染前的连续点击竞态。 */
  const mcpOauthFlowRef = useRef<McpOauthFlowState | null>(null);
  const [pluginsError, setPluginsError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [pathHint, setPathHint] = useState<string | null>(null);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionErrorSource, setActionErrorSource] = useState<
    "plugin" | "mcp" | null
  >(null);
  const [uninstallTarget, setUninstallTarget] = useState<api.PluginDto | null>(
    null,
  );
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [detailsTitle, setDetailsTitle] = useState("");
  const [detailsBody, setDetailsBody] = useState("");
  const [detailsLoading, setDetailsLoading] = useState(false);
  /** 当前打开配置编辑器的插件。 */
  const [configTarget, setConfigTarget] = useState<api.PluginDto | null>(null);
  /** 当前插件的 userConfig 字段定义与公开值。 */
  const [configResult, setConfigResult] =
    useState<api.PluginUserConfigResult | null>(null);
  /** 配置编辑器中的未提交值。 */
  const [configValues, setConfigValues] = useState<Record<string, unknown>>({});
  /** 敏感字段只有被用户编辑过才提交，避免覆盖已有凭据。 */
  const [configTouched, setConfigTouched] = useState<Set<string>>(new Set());
  /** 配置编辑器加载状态。 */
  const [configLoading, setConfigLoading] = useState(false);
  /** 配置保存状态。 */
  const [configSaving, setConfigSaving] = useState(false);
  /** 配置编辑器错误。 */
  const [configError, setConfigError] = useState<string | null>(null);
  /** 插件列表筛选：全部、已启用或已禁用。 */
  const [pluginFilter, setPluginFilter] = useState<PluginFilter>("all");
  const [installSource, setInstallSource] = useState("");
  const [addOpen, setAddOpen] = useState(false);
  const [addName, setAddName] = useState("");
  const [addCommand, setAddCommand] = useState("");
  const [addArgs, setAddArgs] = useState("");
  const [addEnv, setAddEnv] = useState("");
  const [removeTarget, setRemoveTarget] = useState<api.McpDto | null>(null);
  const [doctorOpen, setDoctorOpen] = useState(false);
  const [doctorLoading, setDoctorLoading] = useState(false);
  const [doctorReport, setDoctorReport] =
    useState<api.McpDoctorReport | null>(null);
  const [doctorError, setDoctorError] = useState<string | null>(null);
  const [doctorFocus, setDoctorFocus] = useState<string | null>(null);

  /** 同步更新 OAuth 长流程引用与界面状态。 */
  const commitMcpOauthFlow = useCallback((next: McpOauthFlowState | null) => {
    mcpOauthFlowRef.current = next;
    setMcpOauthFlow(next);
  }, []);

  /** 设置或清除单个 MCP Server 的 OAuth 界面错误。 */
  const updateMcpOauthError = useCallback(
    (serverName: string, error: string | null) => {
      setMcpOauthErrors((previous) => {
        if (error) {
          if (previous[serverName] === error) return previous;
          return { ...previous, [serverName]: error };
        }
        if (!(serverName in previous)) return previous;
        const next = { ...previous };
        delete next[serverName];
        return next;
      });
    },
    [],
  );

  /** 仅刷新 Peri MCP 运行态，避免 OAuth 事件重载 Skills 与插件。 */
  const refreshMcpRuntime = useCallback(async () => {
    if (!api.isTauri()) {
      setMcpRuntime(null);
      setMcpRuntimeError(null);
      return;
    }
    try {
      const snapshot = await api.mcpRuntimeList();
      setMcpRuntime(snapshot);
      setMcpRuntimeError(null);
    } catch (error) {
      setMcpRuntimeError(String(error));
    }
  }, []);

  const refresh = useCallback(async () => {
    if (!api.isTauri()) {
      setSkills([]);
      setServers([]);
      setMcpRuntime(null);
      setPlugins([]);
      setSkillsError(tr("ext.needTauri"));
      setMcpError(null);
      setMcpRuntimeError(null);
      setPluginsError(null);
      setLoading(false);
      return;
    }
    setLoading(true);
    setSkillsError(null);
    setMcpError(null);
    setMcpRuntimeError(null);
    setPluginsError(null);
    setPathHint(null);
    const cwd = projectPath?.trim() || null;
    const [skillsLoad, mcpLoad, mcpRuntimeLoad, pluginsLoad] = await Promise.all([
      api
        .skillsList(cwd)
        .then((value) => ({ value, error: null as string | null }))
        .catch((e) => ({ value: null, error: String(e) })),
      api
        .inspectMcp()
        .then((value) => ({ value, error: null as string | null }))
        .catch((e) => ({ value: null, error: String(e) })),
      api
        .mcpRuntimeList()
        .then((value) => ({ value, error: null as string | null }))
        .catch((e) => ({ value: null, error: String(e) })),
      api
        .pluginsList()
        .then((value) => ({ value, error: null as string | null }))
        .catch((e) => ({ value: null, error: String(e) })),
    ]);
    setSkills(sortSkillsByName(skillsLoad.value?.skills ?? []));
    setServers(sortMcpByName(mcpLoad.value?.servers ?? []));
    setMcpRuntime(mcpRuntimeLoad.value);
    setPlugins(sortPluginsByName(pluginsLoad.value?.plugins ?? []));
    setSkillsError(skillsLoad.error);
    setMcpError(mcpLoad.error);
    setMcpRuntimeError(mcpRuntimeLoad.error);
    setPluginsError(pluginsLoad.error);
    setLoading(false);
  }, [projectPath, tr]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!api.isTauri()) return;
    let disposed = false;
    let unlisten: (() => void) | null = null;

    void listenAcp("acp://agent-event", (notification) => {
      if (disposed) return;
      const event = parseAgentEvent(notification.params?.event_json ?? "");
      if (!event) return;
      const action = projectMcpOAuthUiAction(event);
      if (!action) return;

      if (action.type === "open_authorization") {
        const previous = mcpOauthFlowRef.current;
        commitMcpOauthFlow({
          serverName: action.serverName,
          phase: "awaiting_callback",
          authorizationUrl: action.authorizationUrl,
          callbackInput:
            previous?.serverName === action.serverName
              ? previous.callbackInput
              : "",
        });
        updateMcpOauthError(action.serverName, null);
        void (async () => {
          try {
            await api.urlOpen(action.authorizationUrl);
          } catch (error) {
            updateMcpOauthError(
              action.serverName,
              `${tr("ext.mcp.oauthOpenFailed")}: ${String(error)}`,
            );
          } finally {
            await refreshMcpRuntime();
          }
        })();
        return;
      }

      if (mcpOauthFlowRef.current?.serverName === action.serverName) {
        commitMcpOauthFlow(null);
      }
      updateMcpOauthError(action.serverName, action.error);
      void refreshMcpRuntime();
    })
      .then((dispose) => {
        if (disposed) {
          dispose();
          return;
        }
        unlisten = dispose;
      })
      .catch((error) => {
        if (!disposed) setMcpRuntimeError(String(error));
      });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [commitMcpOauthFlow, refreshMcpRuntime, tr, updateMcpOauthError]);

  const bannerError = useMemo(
    () =>
      mergeInspectErrors(
        skillsError,
        mergeInspectErrors(mcpError, mcpRuntimeError, null),
        pluginsError,
      ),
    [skillsError, mcpError, mcpRuntimeError, pluginsError],
  );
  const mcpRows = useMemo(
    () => mergeMcpServers(servers, mcpRuntime),
    [mcpRuntime, servers],
  );
  const mcpOffCount = useMemo(
    () => servers.filter((s) => !s.enabled).length,
    [servers],
  );
  const reveal = async (path: string | null) => {
    const p = (path ?? "").trim();
    if (!p || !api.isTauri()) return;
    try {
      await api.pathReveal(p);
      setPathHint(null);
    } catch (e) {
      setPathHint(String(e));
    }
  };

  const toggleMcp = async (name: string, next: boolean) => {
    if (!api.isTauri() || busyKey) return;
    setBusyKey(`mcp:${name}`);
    setServers((prev) =>
      prev.map((s) => (s.name === name ? { ...s, enabled: next } : s)),
    );
    try {
      await api.extensionsSetMcp(name, next);
    } catch (e) {
      setPathHint(String(e));
      setServers((prev) =>
        prev.map((s) => (s.name === name ? { ...s, enabled: !next } : s)),
      );
    } finally {
      setBusyKey(null);
    }
  };

  const enableAllMcp = async () => {
    if (!api.isTauri() || busyKey || servers.length === 0) return;
    setBusyKey("mcp:all");
    const names = servers.map((s) => s.name);
    setServers((prev) => prev.map((s) => ({ ...s, enabled: true })));
    try {
      await api.extensionsEnableAllMcp(names);
    } catch (e) {
      setPathHint(String(e));
      await refresh();
    } finally {
      setBusyKey(null);
    }
  };

  /** 显式启动指定 MCP Server 的 OAuth 授权流程。 */
  const startMcpOauth = async (serverName: string) => {
    if (!api.isTauri() || mcpOauthFlowRef.current || busyKey || actionBusy) {
      return;
    }
    commitMcpOauthFlow({
      serverName,
      phase: "starting",
      authorizationUrl: null,
      callbackInput: "",
    });
    updateMcpOauthError(serverName, null);
    try {
      await api.mcpOauthStart(serverName);
      await refreshMcpRuntime();
    } catch (error) {
      updateMcpOauthError(serverName, String(error));
      if (
        (mcpOauthFlowRef.current as McpOauthFlowState | null)?.serverName ===
        serverName
      ) {
        commitMcpOauthFlow(null);
      }
    }
  };

  /** 更新当前 OAuth 流程的手动回调输入。 */
  const updateMcpOauthCallbackInput = (serverName: string, value: string) => {
    const current = mcpOauthFlowRef.current;
    if (!current || current.serverName !== serverName) return;
    commitMcpOauthFlow({ ...current, callbackInput: value });
  };

  /** 重新打开当前 OAuth 流程的授权页面。 */
  const reopenMcpOauthAuthorization = async (serverName: string) => {
    const current = mcpOauthFlowRef.current;
    if (
      !api.isTauri() ||
      !current?.authorizationUrl ||
      current.serverName !== serverName
    ) {
      return;
    }
    updateMcpOauthError(serverName, null);
    try {
      await api.urlOpen(current.authorizationUrl);
    } catch (error) {
      updateMcpOauthError(
        serverName,
        `${tr("ext.mcp.oauthOpenFailed")}: ${String(error)}`,
      );
    }
  };

  /** 提交用户粘贴的 OAuth 回调 URL、查询串或授权码。 */
  const submitMcpOauthCallback = async (serverName: string) => {
    const current = mcpOauthFlowRef.current;
    if (
      !api.isTauri() ||
      !current ||
      current.serverName !== serverName ||
      current.phase !== "awaiting_callback"
    ) {
      return;
    }
    const callback = parseMcpOAuthCallbackInput(
      current.callbackInput,
      current.authorizationUrl,
    );
    if (!callback.ok) {
      updateMcpOauthError(
        serverName,
        mcpOAuthCallbackErrorLabel(tr, callback.error),
      );
      return;
    }

    const submitting = { ...current, phase: "submitting" as const };
    commitMcpOauthFlow(submitting);
    updateMcpOauthError(serverName, null);
    try {
      await api.mcpOauthCallback(serverName, callback.code, callback.state);
      await refreshMcpRuntime();
    } catch (error) {
      updateMcpOauthError(serverName, String(error));
      if (mcpOauthFlowRef.current === submitting) {
        commitMcpOauthFlow({ ...submitting, phase: "awaiting_callback" });
      }
    }
  };

  /** 取消指定 MCP Server 尚未完成的 OAuth 授权。 */
  const cancelMcpOauth = async (serverName: string) => {
    const current = mcpOauthFlowRef.current;
    if (
      !api.isTauri() ||
      !current ||
      current.serverName !== serverName ||
      current.phase !== "awaiting_callback" ||
      busyKey ||
      actionBusy
    ) {
      return;
    }
    const canceling = { ...current, phase: "canceling" as const };
    commitMcpOauthFlow(canceling);
    updateMcpOauthError(serverName, null);
    try {
      await api.mcpOauthCancel(serverName);
      await refreshMcpRuntime();
    } catch (error) {
      updateMcpOauthError(serverName, String(error));
      if (mcpOauthFlowRef.current === canceling) {
        commitMcpOauthFlow({ ...canceling, phase: "awaiting_callback" });
      }
    }
  };

  const runPluginAction = async (
    key: string,
    action: () => Promise<unknown>,
  ) => {
    setActionBusy(key);
    setActionError(null);
    setActionErrorSource(null);
    try {
      await action();
      await refresh();
    } catch (e) {
      setActionError(String(e));
      setActionErrorSource("plugin");
    } finally {
      setActionBusy(null);
    }
  };

  const togglePlugin = (p: api.PluginDto) => {
    const key = pluginRowKey(p);
    void runPluginAction(key, async () => {
      if (p.enabled) {
        await api.pluginDisable(p.name);
      } else {
        await api.pluginEnable(p.name);
      }
    });
  };

  const confirmUninstall = async () => {
    const target = uninstallTarget;
    if (!target) return;
    const key = pluginRowKey(target);
    setUninstallTarget(null);
    await runPluginAction(key, async () => {
      await api.pluginUninstall(target.name);
    });
  };

  const installPlugin = async () => {
    if (!api.isTauri() || actionBusy) return;
    const source = normalizePluginInstallSource(installSource);
    if (!source) {
      setActionError(tr("ext.plugins.installEmpty"));
      return;
    }
    await runPluginAction("install", async () => {
      await api.pluginInstall(source);
      setInstallSource("");
    });
  };

  const updatePlugin = (p: api.PluginDto) => {
    const key = `update:${pluginRowKey(p)}`;
    void runPluginAction(key, async () => {
      await api.pluginUpdate(p.name);
    });
  };

  const updateAllPlugins = () => {
    if (!api.isTauri() || actionBusy || plugins.length === 0) {
      return;
    }
    void runPluginAction("update:all", async () => {
      await api.pluginUpdate(null);
    });
  };

  const showDetails = async (p: api.PluginDto) => {
    setDetailsTitle(p.name);
    setDetailsBody("");
    setDetailsOpen(true);
    setDetailsLoading(true);
    setActionError(null);
    try {
      const res = await api.pluginDetails(p.name);
      setDetailsBody(res.details.trim() || tr("ext.plugins.detailsEmpty"));
    } catch (e) {
      setDetailsBody(String(e));
    } finally {
      setDetailsLoading(false);
    }
  };

  /** 打开插件 userConfig 编辑器，并仅拉取当前插件的配置定义。 */
  const openPluginConfig = async (p: api.PluginDto) => {
    if (!api.isTauri()) return;
    setConfigTarget(p);
    setConfigResult(null);
    setConfigValues({});
    setConfigTouched(new Set());
    setConfigError(null);
    setConfigLoading(true);
    try {
      const result = await api.pluginUserConfigGet(p.name);
      setConfigResult(result);
      setConfigValues(buildConfigEditorValues(result.fields));
    } catch (e) {
      setConfigError(String(e));
    } finally {
      setConfigLoading(false);
    }
  };

  /** 关闭插件配置编辑器；保存进行中不允许丢弃当前表单。 */
  const closePluginConfig = () => {
    if (configSaving) return;
    setConfigTarget(null);
    setConfigResult(null);
    setConfigValues({});
    setConfigTouched(new Set());
    setConfigError(null);
  };

  /** 更新一个字段并记录敏感字段已被用户明确编辑。 */
  const updateConfigValue = (name: string, value: unknown) => {
    setConfigValues((previous) => ({ ...previous, [name]: value }));
    setConfigTouched((previous) => {
      const next = new Set(previous);
      next.add(name);
      return next;
    });
  };

  /** 使用桌面原生选择器设置 directory/file userConfig 值。 */
  const pickConfigPath = async (field: api.PluginUserConfigFieldDto) => {
    if (!api.isTauri()) return;
    try {
      const paths =
        field.valueType === "directory"
          ? await pickDirectoriesForConfig(field)
          : await api.pickAttachFiles();
      if (!paths.length) return;
      const current = configValues[field.name];
      if (field.multiple) {
        const next = [...configPathValues(current), ...paths];
        updateConfigValue(field.name, dedupePaths(next));
      } else {
        updateConfigValue(field.name, paths[0]);
      }
    } catch (e) {
      setConfigError(String(e));
    }
  };

  /** 保存配置后后端会热刷新运行时；随后刷新插件列表投影。 */
  const savePluginConfig = async () => {
    const target = configTarget;
    const result = configResult;
    if (!target || !result || configSaving || !api.isTauri()) return;
    setConfigSaving(true);
    setConfigError(null);
    try {
      const values: Record<string, unknown> = {};
      for (const field of result.fields) {
        const edited = configValues[field.name];
        if (
          field.sensitive &&
          !configTouched.has(field.name) &&
          isEmptyConfigValue(edited)
        ) {
          // 后端不会返回敏感值；未编辑时省略，避免覆盖已有 SecretStore 值。
          continue;
        }
        const normalized = normalizeConfigValue(field, edited);
        if (normalized === undefined) continue;
        values[field.name] = normalized;
      }
      const saved = await api.pluginUserConfigSet(target.name, values, false);
      setConfigResult(saved);
      setConfigValues(buildConfigEditorValues(saved.fields));
      setConfigTouched(new Set());
      await refresh();
    } catch (e) {
      setConfigError(String(e));
    } finally {
      setConfigSaving(false);
    }
  };

  const resetAddForm = () => {
    setAddName("");
    setAddCommand("");
    setAddArgs("");
    setAddEnv("");
  };

  const openAdd = () => {
    resetAddForm();
    setActionError(null);
    setAddOpen(true);
  };

  const submitAdd = async () => {
    if (!api.isTauri() || actionBusy) return;
    const name = addName.trim();
    const command = addCommand.trim();
    if (!name || !command) return;
    const args = splitArgs(addArgs);
    const env = parseEnvLines(addEnv);
    setActionBusy("mcp:add");
    setActionError(null);
    setActionErrorSource(null);
    try {
      await api.mcpAdd({
        name,
        command,
        args,
        env: Object.keys(env).length ? env : undefined,
      });
      setAddOpen(false);
      resetAddForm();
      await refresh();
    } catch (e) {
      setActionError(String(e));
      setActionErrorSource("mcp");
    } finally {
      setActionBusy(null);
    }
  };

  const confirmRemoveMcp = async () => {
    const target = removeTarget;
    if (!target || !api.isTauri()) return;
    setRemoveTarget(null);
    setActionBusy(`mcp:rm:${target.name}`);
    setActionError(null);
    setActionErrorSource(null);
    try {
      await api.mcpRemove(target.name);
      await refresh();
    } catch (e) {
      setActionError(String(e));
      setActionErrorSource("mcp");
    } finally {
      setActionBusy(null);
    }
  };

  const runDoctor = useCallback(
    async (focusName?: string | null) => {
      if (!api.isTauri()) return;
      setDoctorOpen(true);
      setDoctorLoading(true);
      setDoctorError(null);
      setDoctorFocus(focusName?.trim() || null);
      try {
        const report = await api.mcpDoctor(focusName?.trim() || null);
        setDoctorReport(report);
      } catch (e) {
        setDoctorReport(null);
        setDoctorError(String(e));
      } finally {
        setDoctorLoading(false);
      }
    },
    [],
  );

  const visiblePlugins = useMemo(
    () => filterPluginsByLoadState(plugins, pluginFilter),
    [plugins, pluginFilter],
  );

  const tab = activeTab;

  return (
    <div className="ext-panel" data-testid="extensions-panel">
      <p className="settings-page__lead">{tr("ext.lead")}</p>

      {onTabChange ? (
        <div
          className="settings-account-tabs settings-page__tabs"
          role="tablist"
          aria-label={tr("settings.nav.extensions")}
        >
          <div
            className="settings-seg settings-seg--lg settings-page__tabs-seg"
            role="presentation"
          >
            {(
              [
                ["market", "ext.market.title"],
                ["skills", "ext.skills.title"],
                ["mcp", "ext.mcp.title"],
              ] as const
            ).map(([id, key]) => (
              <button
                key={id}
                type="button"
                role="tab"
                className={"settings-seg__btn" + (tab === id ? " is-on" : "")}
                aria-selected={tab === id}
                onClick={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  onTabChange(id);
                }}
              >
                {tr(key)}
              </button>
            ))}
          </div>
        </div>
      ) : null}

      {pathHint && (
        <p className="ext-alert ext-alert--warn" role="status">
          {pathHint}
        </p>
      )}

      {actionError && (
        <div className="ext-alert ext-alert--error" role="alert">
          <div className="ext-alert__title">
            {actionErrorSource === "mcp"
              ? tr("ext.mcp.actionError")
              : tr("ext.plugins.actionError")}
          </div>
          <p className="ext-alert__body">{actionError}</p>
          <button
            type="button"
            className="btn btn--ghost ext-alert__cta"
            onClick={() => {
              setActionError(null);
              setActionErrorSource(null);
            }}
          >
            {tr("common.close")}
          </button>
        </div>
      )}

      {bannerError && (
        <div className="ext-alert ext-alert--warn" role="alert">
          <div className="ext-alert__title">{tr("ext.error.title")}</div>
          <p className="ext-alert__body">{bannerError}</p>
        </div>
      )}

      {tab === "market" && (
        <ExtensionsBuildExtras
          locale={locale}
          onPluginsChanged={() => {
            void refresh();
          }}
        />
      )}

      {/* 插件市场页同时展示已安装插件，避免再增加一级菜单。 */}
      {tab === "market" && (
      <>
      <h2 className="settings-page__h2" id="settings-anchor-ext-plugins">
        <IconPuzzle size={15} />
        {tr("ext.plugins.title")}
        {!loading ? (
          <span className="ext-count">{plugins.length}</span>
        ) : null}
        {!loading && plugins.length > 0 ? (
          <button
            type="button"
            className="btn btn--ghost ext-bulk-btn"
            disabled={!!actionBusy || !!busyKey}
            onClick={() => updateAllPlugins()}
          >
            {actionBusy === "update:all"
              ? tr("ext.plugins.updating")
              : tr("ext.plugins.updateAll")}
          </button>
        ) : null}
      </h2>
      <div className="settings-card ext-card">
        {!loading && plugins.length > 0 ? (
          <div
            className="ext-plugin-filters"
            role="tablist"
            aria-label={tr("ext.plugins.filterLabel")}
          >
            {(
              [
                ["all", "ext.plugins.filter.all"],
                ["enabled", "ext.plugins.filter.enabled"],
                ["disabled", "ext.plugins.filter.disabled"],
              ] as const
            ).map(([id, key]) => (
              <button
                key={id}
                type="button"
                role="tab"
                aria-selected={pluginFilter === id}
                className={
                  "ext-plugin-filter" + (pluginFilter === id ? " is-active" : "")
                }
                onClick={() => setPluginFilter(id)}
              >
                {tr(key)}
              </button>
            ))}
          </div>
        ) : null}
        {loading && <p className="ext-empty">{tr("ext.plugins.loading")}</p>}
        {!loading && plugins.length === 0 && (
          <div className="ext-empty-cta">
            <p className="ext-empty-cta__text">
              {tr("ext.plugins.empty")}
            </p>
          </div>
        )}
        {!loading && plugins.length > 0 && visiblePlugins.length === 0 && (
          <p className="ext-empty">{tr("ext.plugins.filterEmpty")}</p>
        )}
        {!loading && visiblePlugins.length > 0 && (
          <ul className="ext-list">
            {visiblePlugins.map((p) => {
              const key = pluginRowKey(p);
              const rowBusy = actionBusy === key;
              const updating = actionBusy === `update:${key}`;
              const busy = rowBusy || updating;
              const tone = pluginStatusTone(p.enabled);
              const provides = pluginProvidesLine(p);
              const unsupportedHooks = pluginUnsupportedHooksLine(p);
              const hasLspServers = pluginLspRequiresRestart(p);
              return (
                <li
                  key={key}
                  className={
                    "ext-item" + (p.enabled ? "" : " ext-item--disabled")
                  }
                >
                  <div className="ext-item__head">
                    <strong className="ext-item__name">{p.name}</strong>
                    <span className={`ext-badge ext-badge--plugin-${tone}`}>
                      {p.enabled
                        ? tr("ext.plugins.status.enabled")
                        : tr("ext.plugins.status.disabled")}
                    </span>
                    {p.version ? (
                      <span className="ext-badge ext-badge--muted">
                        v{String(p.version).replace(/^v/i, "")}
                      </span>
                    ) : null}
                  </div>
                  {provides ? (
                    <p className="ext-item__desc ext-item__provides">{provides}</p>
                  ) : null}
                  {hasLspServers ? (
                    <p className="ext-item__desc">
                      {tr("ext.plugins.lspRestart")}
                    </p>
                  ) : null}
                  {unsupportedHooks ? (
                    <p className="ext-item__desc ext-item__warn">{unsupportedHooks}</p>
                  ) : null}
                  <div className="ext-item__meta">
                    {p.marketplace ? (
                      <span>
                        {tr("ext.plugins.marketplace")}: {p.marketplace}
                      </span>
                    ) : null}
                    <button
                      type="button"
                      className="ext-path-btn"
                      title={p.path}
                      onClick={() => void reveal(p.path)}
                    >
                      <IconFolder size={13} />
                      <span>{shortPathLabel(p.path, 42)}</span>
                    </button>
                  </div>
                  <div className="ext-item__actions">
                    <button
                      type="button"
                      className="btn btn--ghost btn--sm"
                      disabled={busy || !!actionBusy}
                      onClick={() => togglePlugin(p)}
                    >
                      {rowBusy
                        ? tr("ext.plugins.working")
                        : p.enabled
                          ? tr("ext.plugins.disable")
                          : tr("ext.plugins.enable")}
                    </button>
                    <button
                      type="button"
                      className="btn btn--ghost btn--sm"
                      disabled={busy || !!actionBusy}
                      onClick={() => updatePlugin(p)}
                    >
                      {updating
                        ? tr("ext.plugins.updating")
                        : tr("ext.plugins.update")}
                    </button>
                    <button
                      type="button"
                      className="btn btn--ghost btn--sm"
                      disabled={busy || !!actionBusy}
                      onClick={() => void showDetails(p)}
                    >
                      {tr("ext.plugins.details")}
                    </button>
                    <button
                      type="button"
                      className="btn btn--ghost btn--sm"
                      disabled={busy || !!actionBusy || configLoading}
                      onClick={() => void openPluginConfig(p)}
                    >
                      {tr("ext.plugins.configure")}
                    </button>
                    <button
                      type="button"
                      className="btn btn--ghost btn--sm ext-item__danger"
                      disabled={busy || !!actionBusy}
                      onClick={() => setUninstallTarget(p)}
                    >
                      <IconTrash size={13} />
                      <span>{tr("ext.plugins.uninstall")}</span>
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
        <details className="ext-market-sources">
            <summary className="ext-market-sources__summary">
              {tr("ext.plugins.advancedInstall")}
            </summary>
            <div className="ext-plugin-install">
              <label
                className="ext-plugin-install__label"
                htmlFor="ext-plugin-source"
              >
                {tr("ext.plugins.installLabel")}
              </label>
              <div className="ext-plugin-install__row">
                <input
                  id="ext-plugin-source"
                  type="text"
                  className="settings-input ext-plugin-install__input"
                  value={installSource}
                  placeholder={tr("ext.plugins.installPlaceholder")}
                  disabled={!!actionBusy}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(e) => setInstallSource(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      void installPlugin();
                    }
                  }}
                />
                <button
                  type="button"
                  className="btn btn--solid btn--sm"
                  disabled={
                    !!actionBusy ||
                    !normalizePluginInstallSource(installSource)
                  }
                  onClick={() => void installPlugin()}
                >
                  {actionBusy === "install"
                    ? tr("ext.plugins.installing")
                    : tr("ext.plugins.install")}
                </button>
              </div>
            </div>
        </details>
      </div>
      </>
      )}

      {/* Skills */}
      {tab === "skills" && (
      <>
      <h2 className="settings-page__h2" id="settings-anchor-ext-skills">
        <IconSkills size={15} />
        {tr("ext.skills.title")}
        {!loading ? (
          <span className="ext-count">{skills.length}</span>
        ) : null}
      </h2>
      <div className="settings-card ext-card">
        {loading && (
          <p className="ext-empty">{tr("ext.skills.loading")}</p>
        )}
        {!loading && skills.length === 0 && (
          <p className="ext-empty">{tr("ext.skills.empty")}</p>
        )}
        {!loading && skills.length > 0 && (
          <ul className="ext-list">
            {skills.map((s) => {
              const tone = skillSourceTone(s.source);
              return (
                <li
                  key={`${s.source}:${s.name}:${s.path}`}
                  className="ext-item"
                >
                  <div className="ext-item__head">
                    <strong className="ext-item__name">{s.name}</strong>
                    <span className={`ext-badge ext-badge--${tone}`}>
                      {s.source}
                    </span>
                    {s.userInvocable ? (
                      <span className="ext-badge ext-badge--invocable">
                        {tr("ext.skills.invocable")}
                      </span>
                    ) : null}
                  </div>
                  {s.description ? (
                    <p className="ext-item__desc">{s.description}</p>
                  ) : null}
                  <div className="ext-item__meta">
                    <span>{skillMetaLine(s)}</span>
                    <button
                      type="button"
                      className="ext-path-btn"
                      title={s.path}
                      onClick={() => void reveal(s.path)}
                    >
                      <IconFolder size={13} />
                      <span>{shortPathLabel(s.path, 42)}</span>
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
      </>
      )}

      {/* MCP */}
      {tab === "mcp" && (
      <>
      <h2 className="settings-page__h2" id="settings-anchor-ext-mcp">
        <IconPlug size={15} />
        {tr("ext.mcp.title")}
        {!loading ? (
          <span className="ext-count">{mcpRows.length}</span>
        ) : null}
        {mcpRuntime ? (
          <span
            className={`ext-badge ext-badge--${mcpRuntimePhaseTone(mcpRuntime.initPhase)}`}
          >
            {mcpRuntimePhaseLabel(tr, mcpRuntime.initPhase)}
          </span>
        ) : null}
        <span className="ext-h2-actions">
          <button
            type="button"
            className="btn btn--ghost ext-bulk-btn"
            disabled={!!actionBusy || !!busyKey || !!mcpOauthFlow}
            onClick={() => void runDoctor(null)}
          >
            <IconDoctor size={14} />
            <span>{tr("ext.mcp.doctor")}</span>
          </button>
          <button
            type="button"
            className="btn btn--ghost ext-bulk-btn"
            disabled={
              !!actionBusy || !!busyKey || !!mcpOauthFlow || !api.isTauri()
            }
            onClick={openAdd}
          >
            <IconPlus size={14} />
            <span>{tr("ext.mcp.add")}</span>
          </button>
          {!loading && servers.length > 0 && mcpOffCount > 0 ? (
            <button
              type="button"
              className="btn btn--ghost ext-bulk-btn"
              disabled={!!busyKey || !!actionBusy || !!mcpOauthFlow}
              onClick={() => void enableAllMcp()}
            >
              {tr("ext.enableAll")}
            </button>
          ) : null}
        </span>
      </h2>
      <div className="settings-card ext-card">
        {loading && <p className="ext-empty">{tr("ext.mcp.loading")}</p>}
        {!loading && mcpRows.length === 0 && (
          <p className="ext-empty">{tr("ext.mcp.empty")}</p>
        )}
        {!loading && mcpRows.length > 0 && (
          <ul className="ext-list">
            {mcpRows.map((s) => {
              const on = s.enabled;
              const rmBusy = actionBusy === `mcp:rm:${s.name}`;
              const oauthFlow =
                mcpOauthFlow?.serverName === s.name ? mcpOauthFlow : null;
              return (
                <li
                  key={s.name}
                  className={"ext-item" + (on ? "" : " ext-item--off")}
                >
                  <div className="ext-item__head">
                    <strong className="ext-item__name">{s.name}</strong>
                    {s.config ? (
                      <ExtensionToggle
                        checked={on}
                        disabled={
                          !!busyKey || !!actionBusy || !!mcpOauthFlow
                        }
                        label={on ? tr("ext.enabled") : tr("ext.disabled")}
                        onChange={(next) => void toggleMcp(s.name, next)}
                      />
                    ) : null}
                  </div>
                  <McpRuntimeDetails
                    locale={locale}
                    server={s}
                    error={mcpOauthErrors[s.name] ?? s.error}
                  />
                  {s.target ? (
                    <div className="ext-item__meta">
                      <em className="ext-item__target" title={s.target}>
                        {shortPathLabel(s.target, 64)}
                      </em>
                      {looksLikePath(s.target) ? (
                        <button
                          type="button"
                          className="ext-path-btn"
                          title={s.target}
                          onClick={() => void reveal(s.target)}
                        >
                          <IconFolder size={13} />
                          <span>{tr("ext.reveal")}</span>
                        </button>
                      ) : null}
                    </div>
                  ) : null}
                  <div className="ext-item__actions">
                    {s.oauthStatus === "needs_authorization" &&
                    (!oauthFlow || oauthFlow.phase === "starting") ? (
                      <button
                        type="button"
                        className="btn btn--primary btn--sm"
                        disabled={
                          !!mcpOauthFlow || !!actionBusy || !!busyKey || !on
                        }
                        onClick={() => void startMcpOauth(s.name)}
                      >
                        <IconPlug size={13} />
                        <span>
                          {oauthFlow?.phase === "starting"
                            ? tr("ext.mcp.authorizing")
                            : tr("ext.mcp.authorize")}
                        </span>
                      </button>
                    ) : null}
                    <button
                      type="button"
                      className="btn btn--ghost btn--sm"
                      disabled={
                        !!actionBusy || doctorLoading || !!mcpOauthFlow
                      }
                      onClick={() => void runDoctor(s.name)}
                    >
                      <IconDoctor size={13} />
                      <span>{tr("ext.mcp.doctor")}</span>
                    </button>
                    {s.config ? (
                      <button
                        type="button"
                        className="btn btn--ghost btn--sm ext-item__danger"
                        disabled={rmBusy || !!actionBusy || !!mcpOauthFlow}
                        onClick={() => setRemoveTarget(s.config)}
                      >
                        <IconTrash size={13} />
                        <span>
                          {rmBusy
                            ? tr("ext.plugins.working")
                            : tr("ext.mcp.remove")}
                        </span>
                      </button>
                    ) : null}
                    {oauthFlow && oauthFlow.phase !== "starting" ? (
                      <div className="ext-mcp-oauth-callback">
                        <label
                          className="ext-mcp-oauth-callback__label"
                          htmlFor={`ext-mcp-oauth-callback-${s.name}`}
                        >
                          {tr("ext.mcp.oauthCallback.label")}
                        </label>
                        <div className="ext-mcp-oauth-callback__row">
                          <input
                            id={`ext-mcp-oauth-callback-${s.name}`}
                            type="text"
                            className="settings-input ext-mcp-oauth-callback__input"
                            value={oauthFlow.callbackInput}
                            placeholder={tr(
                              "ext.mcp.oauthCallback.placeholder",
                            )}
                            disabled={oauthFlow.phase !== "awaiting_callback"}
                            autoComplete="off"
                            spellCheck={false}
                            onChange={(event) =>
                              updateMcpOauthCallbackInput(
                                s.name,
                                event.currentTarget.value,
                              )
                            }
                          />
                          <button
                            type="button"
                            className="btn btn--primary btn--sm"
                            disabled={
                              oauthFlow.phase !== "awaiting_callback" ||
                              !oauthFlow.callbackInput.trim()
                            }
                            onClick={() =>
                              void submitMcpOauthCallback(s.name)
                            }
                          >
                            {oauthFlow.phase === "submitting"
                              ? tr("ext.mcp.oauthCallback.submitting")
                              : tr("ext.mcp.oauthCallback.submit")}
                          </button>
                          <button
                            type="button"
                            className="btn btn--ghost btn--sm"
                            disabled={oauthFlow.phase !== "awaiting_callback"}
                            onClick={() => void cancelMcpOauth(s.name)}
                          >
                            {oauthFlow.phase === "canceling"
                              ? tr("ext.mcp.canceling")
                              : tr("ext.mcp.cancelAuth")}
                          </button>
                        </div>
                        <div className="ext-mcp-oauth-callback__hint">
                          <span>{tr("ext.mcp.oauthCallback.hint")}</span>
                          <button
                            type="button"
                            className="ext-path-btn"
                            disabled={!oauthFlow.authorizationUrl}
                            onClick={() =>
                              void reopenMcpOauthAuthorization(s.name)
                            }
                          >
                            {tr("ext.mcp.oauthCallback.reopen")}
                          </button>
                        </div>
                      </div>
                    ) : null}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
      </>
      )}

      <GlassModal
        open={!!uninstallTarget}
        onClose={() => {
          if (!actionBusy) setUninstallTarget(null);
        }}
        title={tr("ext.plugins.uninstallTitle")}
        size="sm"
        closeLabel={tr("common.close")}
        footer={
          <>
            <button
              type="button"
              className="btn btn--ghost"
              disabled={!!actionBusy}
              onClick={() => setUninstallTarget(null)}
            >
              {tr("common.cancel")}
            </button>
            <button
              type="button"
              className="btn btn--danger"
              disabled={!!actionBusy}
              onClick={() => void confirmUninstall()}
            >
              {tr("ext.plugins.uninstall")}
            </button>
          </>
        }
      >
        <p className="app-dialog__msg">
          {tr(
            uninstallTarget && pluginLspRequiresRestart(uninstallTarget)
              ? "ext.plugins.uninstallConfirmLsp"
              : "ext.plugins.uninstallConfirm",
            {
              name: uninstallTarget?.name ?? "",
            },
          )}
        </p>
      </GlassModal>

      <GlassModal
        open={detailsOpen}
        onClose={() => setDetailsOpen(false)}
        title={tr("ext.plugins.detailsTitle", { name: detailsTitle })}
        size="lg"
        closeLabel={tr("common.close")}
        wrapBody
        footer={
          <button
            type="button"
            className="btn btn--ghost"
            onClick={() => setDetailsOpen(false)}
          >
            {tr("common.close")}
          </button>
        }
      >
        {detailsLoading ? (
          <p className="ext-empty">{tr("ext.plugins.detailsLoading")}</p>
        ) : (
          <pre className="ext-details-pre">{detailsBody}</pre>
        )}
      </GlassModal>

      <GlassModal
        open={!!configTarget}
        onClose={closePluginConfig}
        title={tr("ext.plugins.configureTitle", {
          name: configTarget?.name ?? "",
        })}
        size="lg"
        closeLabel={tr("common.close")}
        wrapBody
        footer={
          <>
            <button
              type="button"
              className="btn btn--ghost"
              disabled={configSaving}
              onClick={closePluginConfig}
            >
              {tr("common.cancel")}
            </button>
            <button
              type="button"
              className="btn btn--solid"
              disabled={configLoading || configSaving || !configResult}
              onClick={() => void savePluginConfig()}
            >
              {configSaving
                ? tr("ext.plugins.configSaving")
                : tr("ext.plugins.configSave")}
            </button>
          </>
        }
      >
        {configLoading ? (
          <p className="ext-empty">{tr("ext.plugins.configLoading")}</p>
        ) : configError ? (
          <div className="ext-alert ext-alert--error" role="alert">
            <p className="ext-alert__body">{configError}</p>
          </div>
        ) : configResult && configResult.fields.length === 0 ? (
          <p className="ext-empty">{tr("ext.plugins.configEmpty")}</p>
        ) : configResult ? (
          <form
            className="app-dialog__form"
            onSubmit={(event) => {
              event.preventDefault();
              void savePluginConfig();
            }}
          >
            {configResult.fields.map((field) => (
              <PluginUserConfigEditor
                key={field.name}
                field={field}
                value={configValues[field.name]}
                onChange={(value) => updateConfigValue(field.name, value)}
                onPickPath={() => void pickConfigPath(field)}
                tr={tr}
              />
            ))}
            <p className="ext-field-hint">
              {tr(
                configTarget && pluginLspRequiresRestart(configTarget)
                  ? "ext.plugins.configLspRestart"
                  : "ext.plugins.configImmediate",
              )}
            </p>
          </form>
        ) : null}
      </GlassModal>

      <GlassModal
        open={addOpen}
        onClose={() => {
          if (actionBusy !== "mcp:add") setAddOpen(false);
        }}
        title={tr("ext.mcp.addTitle")}
        size="md"
        closeLabel={tr("common.close")}
        wrapBody
        footer={
          <>
            <button
              type="button"
              className="btn btn--ghost"
              disabled={actionBusy === "mcp:add"}
              onClick={() => setAddOpen(false)}
            >
              {tr("common.cancel")}
            </button>
            <button
              type="button"
              className="btn btn--solid"
              disabled={
                actionBusy === "mcp:add" ||
                !addName.trim() ||
                !addCommand.trim()
              }
              onClick={() => void submitAdd()}
            >
              {actionBusy === "mcp:add"
                ? tr("ext.mcp.addWorking")
                : tr("ext.mcp.addSubmit")}
            </button>
          </>
        }
      >
        <form
          className="app-dialog__form"
          onSubmit={(e) => {
            e.preventDefault();
            void submitAdd();
          }}
        >
          <label className="field">
            <span>{tr("ext.mcp.name")}</span>
            <input
              className="app-dialog__input"
              value={addName}
              onChange={(e) => setAddName(e.target.value)}
              placeholder={tr("ext.mcp.namePlaceholder")}
              autoComplete="off"
              spellCheck={false}
              disabled={actionBusy === "mcp:add"}
            />
          </label>
          <label className="field">
            <span>{tr("ext.mcp.command")}</span>
            <input
              className="app-dialog__input"
              value={addCommand}
              onChange={(e) => setAddCommand(e.target.value)}
              placeholder={tr("ext.mcp.commandPlaceholder")}
              autoComplete="off"
              spellCheck={false}
              disabled={actionBusy === "mcp:add"}
            />
          </label>
          <label className="field">
            <span>{tr("ext.mcp.args")}</span>
            <input
              className="app-dialog__input"
              value={addArgs}
              onChange={(e) => setAddArgs(e.target.value)}
              placeholder={tr("ext.mcp.argsPlaceholder")}
              autoComplete="off"
              spellCheck={false}
              disabled={actionBusy === "mcp:add"}
            />
            <span className="ext-field-hint">{tr("ext.mcp.argsHint")}</span>
          </label>
          <label className="field">
            <span>{tr("ext.mcp.env")}</span>
            <textarea
              className="app-dialog__input ext-env-textarea"
              value={addEnv}
              onChange={(e) => setAddEnv(e.target.value)}
              placeholder={tr("ext.mcp.envPlaceholder")}
              rows={3}
              spellCheck={false}
              disabled={actionBusy === "mcp:add"}
            />
            <span className="ext-field-hint">{tr("ext.mcp.envHint")}</span>
          </label>
        </form>
      </GlassModal>

      <GlassModal
        open={!!removeTarget}
        onClose={() => {
          if (!actionBusy) setRemoveTarget(null);
        }}
        title={tr("ext.mcp.removeTitle")}
        size="sm"
        closeLabel={tr("common.close")}
        footer={
          <>
            <button
              type="button"
              className="btn btn--ghost"
              disabled={!!actionBusy}
              onClick={() => setRemoveTarget(null)}
            >
              {tr("common.cancel")}
            </button>
            <button
              type="button"
              className="btn btn--danger"
              disabled={!!actionBusy}
              onClick={() => void confirmRemoveMcp()}
            >
              {tr("ext.mcp.remove")}
            </button>
          </>
        }
      >
        <p className="app-dialog__msg">
          {tr("ext.mcp.removeConfirm", {
            name: removeTarget?.name ?? "",
          })}
        </p>
      </GlassModal>

      <GlassModal
        open={doctorOpen}
        onClose={() => {
          if (!doctorLoading) setDoctorOpen(false);
        }}
        title={
          doctorFocus
            ? `${tr("ext.mcp.doctorTitle")} · ${doctorFocus}`
            : tr("ext.mcp.doctorTitle")
        }
        size="lg"
        closeLabel={tr("common.close")}
        wrapBody
        footer={
          <>
            <button
              type="button"
              className="btn btn--ghost"
              disabled={doctorLoading}
              onClick={() => void runDoctor(doctorFocus)}
            >
              <IconRefresh size={14} />
              <span>{tr("ext.mcp.doctorRerun")}</span>
            </button>
            <button
              type="button"
              className="btn btn--ghost"
              disabled={doctorLoading}
              onClick={() => setDoctorOpen(false)}
            >
              {tr("common.close")}
            </button>
          </>
        }
      >
        {doctorLoading && (
          <p className="ext-empty">{tr("ext.mcp.doctorRunning")}</p>
        )}
        {!doctorLoading && doctorError && (
          <div className="ext-alert ext-alert--error" role="alert">
            <p className="ext-alert__body">{doctorError}</p>
          </div>
        )}
        {!doctorLoading && doctorReport && (
          <div className="ext-doctor">
            <p className="ext-doctor__summary">
              {tr("ext.mcp.doctorSummary", {
                healthy: doctorReport.summary.healthy,
                unhealthy: doctorReport.summary.unhealthy,
                total: doctorReport.summary.total,
              })}
            </p>
            {doctorReport.sources.length > 0 ? (
              <div className="ext-doctor__sources">
                <div className="ext-doctor__section-title">
                  {tr("ext.mcp.doctorSources")}
                </div>
                <ul className="ext-doctor__source-list">
                  {doctorReport.sources.map((src) => (
                    <li key={src.path}>
                      <code>{src.path}</code>
                      <span className="ext-badge ext-badge--muted">
                        {src.status} · {src.serverCount}
                      </span>
                    </li>
                  ))}
                </ul>
              </div>
            ) : null}
            {doctorReport.servers.length === 0 ? (
              <p className="ext-empty">
                {doctorReport.rawText?.trim() || tr("ext.mcp.doctorEmpty")}
              </p>
            ) : (
              <ul className="ext-list ext-doctor__servers">
                {doctorReport.servers.map((s) => (
                  <li
                    key={s.name}
                    className={
                      "ext-item" + (s.healthy ? "" : " ext-item--off")
                    }
                  >
                    <div className="ext-item__head">
                      <strong className="ext-item__name">{s.name}</strong>
                      <span
                        className={
                          "ext-badge " +
                          (s.healthy
                            ? "ext-badge--ok"
                            : "ext-badge--fail")
                        }
                      >
                        {s.healthy
                          ? tr("ext.mcp.doctorHealthy")
                          : tr("ext.mcp.doctorUnhealthy")}
                      </span>
                      <span className="ext-badge ext-badge--muted">
                        {s.transport}
                      </span>
                    </div>
                    {s.target ? (
                      <p className="ext-item__desc" title={s.target}>
                        {shortPathLabel(s.target, 72)}
                      </p>
                    ) : null}
                    {s.checks.length > 0 ? (
                      <ul className="ext-doctor__checks">
                        {s.checks.map((c, index) => (
                          <li
                            key={`${s.name}:${c.label}:${index}`}
                            className={
                              "ext-doctor__check" +
                              (c.passed ? " is-pass" : " is-fail")
                            }
                          >
                            <span className="ext-doctor__check-label">
                              {c.passed ? "✓" : "✗"} {c.label}
                            </span>
                            {c.detail ? (
                              <span className="ext-doctor__check-detail">
                                {c.detail}
                              </span>
                            ) : null}
                          </li>
                        ))}
                      </ul>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
            {doctorReport.rawText ? (
              <pre className="ext-details-pre">{doctorReport.rawText}</pre>
            ) : null}
          </div>
        )}
      </GlassModal>
    </div>
  );
}

/** Space-separated args; keeps simple tokens (no shell quoting). */
function splitArgs(raw: string): string[] {
  return raw
    .trim()
    .split(/\s+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

/** 为 userConfig 编辑器创建稳定的初始草稿值；敏感字段故意留空。 */
function buildConfigEditorValues(
  fields: api.PluginUserConfigFieldDto[],
): Record<string, unknown> {
  return Object.fromEntries(
    fields.map((field) => {
      const source = field.sensitive ? undefined : field.value ?? field.default;
      return [field.name, editorValueForField(field, source)];
    }),
  );
}

/** 将后端值转换成可由原生表单编辑的标量或数组。 */
function editorValueForField(
  field: api.PluginUserConfigFieldDto,
  value: unknown,
): unknown {
  if (field.multiple) {
    if (Array.isArray(value)) return value;
    return value == null ? [] : [value];
  }
  if (value === undefined && field.sensitive) return "";
  if (field.valueType === "boolean") return value === true;
  if (value == null) return field.valueType === "number" ? "" : "";
  return value;
}

/** 将编辑器草稿校验/转换成 Claude userConfig 接受的 JSON 值。 */
function normalizeConfigValue(
  field: api.PluginUserConfigFieldDto,
  value: unknown,
): unknown {
  if (field.multiple) {
    const values = Array.isArray(value) ? value : value == null ? [] : [value];
    const normalized = values
      .map((item) => normalizeConfigScalar(field, item))
      .filter((item) => item !== undefined);
    return normalized.length || values.length ? normalized : undefined;
  }
  return normalizeConfigScalar(field, value);
}

/** 转换单个字段值；空的可选文本/数字字段不会被提交。 */
function normalizeConfigScalar(
  field: api.PluginUserConfigFieldDto,
  value: unknown,
): unknown {
  if (field.valueType === "boolean") return value === true;
  if (field.valueType === "number") {
    if (value === "" || value == null) return undefined;
    const number = typeof value === "number" ? value : Number(value);
    return Number.isFinite(number) ? number : undefined;
  }
  if (value == null) return undefined;
  if (field.valueType === "select") return value === "" ? undefined : value;
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed ? trimmed : undefined;
  }
  return typeof value === "string" || typeof value === "number" || typeof value === "boolean"
    ? value
    : JSON.stringify(value);
}

/** 判断敏感字段是否仍为空白，避免无意覆盖 SecretStore 中的已有值。 */
function isEmptyConfigValue(value: unknown): boolean {
  return Array.isArray(value)
    ? value.length === 0
    : value == null || (typeof value === "string" && value.trim() === "");
}

/** 读取路径数组；配置文件可能传入单个字符串，统一按数组编辑。 */
function configPathValues(value: unknown): string[] {
  if (Array.isArray(value)) {
    return value.filter((item): item is string => typeof item === "string");
  }
  return typeof value === "string" && value.trim() ? [value] : [];
}

/** 去掉重复路径，保持用户选择顺序。 */
function dedupePaths(paths: string[]): string[] {
  return Array.from(new Set(paths.filter((path) => path.trim())));
}

/** 选择目录一次；多选目录可重复点击按钮追加。 */
async function pickDirectoriesForConfig(
  _field: api.PluginUserConfigFieldDto,
): Promise<string[]> {
  const path = await api.pickDirectory();
  return path ? [path] : [];
}

/** 为 select 候选生成无歧义的原生 option value。 */
function serializeConfigValue(value: unknown): string {
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

/** 将原生 select option value 还原为 JSON 值。 */
function parseSerializedConfigValue(value: string): unknown {
  try {
    return JSON.parse(value) as unknown;
  } catch {
    return value;
  }
}

/** 展示 select 候选的简洁标签，同时保留复杂值的可读 JSON。 */
function displayConfigValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (value == null) return "null";
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

/** 插件 userConfig 的通用设置字段编辑器。 */
function PluginUserConfigEditor({
  field,
  value,
  onChange,
  onPickPath,
  tr,
}: {
  field: api.PluginUserConfigFieldDto;
  value: unknown;
  onChange: (value: unknown) => void;
  onPickPath: () => void;
  tr: ReturnType<typeof createT>;
}) {
  const enumValues = Array.isArray(field.enumValues) ? field.enumValues : [];
  const label = field.title?.trim() || field.name;
  const typeLabel = field.multiple
    ? `${field.valueType}[]`
    : field.valueType;
  const pathField = field.valueType === "directory" || field.valueType === "file";

  const renderSelect = (current: unknown, multiple: boolean) => (
    <select
      className="app-dialog__input"
      multiple={multiple}
      value={
        multiple
          ? (Array.isArray(current) ? current : []).map(serializeConfigValue)
          : current == null || current === ""
            ? ""
            : serializeConfigValue(current)
      }
      onChange={(event) => {
        if (multiple) {
          const selected = Array.from(event.currentTarget.selectedOptions).map((option) =>
            parseSerializedConfigValue(option.value),
          );
          onChange(selected);
        } else {
          onChange(parseSerializedConfigValue(event.currentTarget.value));
        }
      }}
      required={field.required}
      disabled={false}
      size={multiple ? Math.min(Math.max(enumValues.length, 2), 6) : undefined}
    >
      {!multiple && !field.required ? <option value="">{tr("ext.plugins.configUnset")}</option> : null}
      {enumValues.map((option, index) => (
        <option key={`${field.name}:${index}:${serializeConfigValue(option)}`} value={serializeConfigValue(option)}>
          {displayConfigValue(option)}
        </option>
      ))}
    </select>
  );

  const renderScalar = (current: unknown, change: (next: unknown) => void) => {
    if (field.valueType === "boolean") {
      return (
        <span className="field__checkbox">
          <input
            type="checkbox"
            checked={current === true}
            onChange={(event) => change(event.currentTarget.checked)}
          />
          <span>{current === true ? tr("ext.enabled") : tr("ext.disabled")}</span>
        </span>
      );
    }
    if (field.valueType === "select" && enumValues.length > 0) {
      return renderSelect(current, false);
    }
    const inputType = field.sensitive ? "password" : field.valueType === "number" ? "number" : "text";
    return (
      <div className="ext-plugin-config__input-row">
        <input
          className="app-dialog__input"
          type={inputType}
          value={current == null ? "" : String(current)}
          onChange={(event) =>
            change(field.valueType === "number" ? event.currentTarget.value : event.currentTarget.value)
          }
          min={field.valueType === "number" ? field.min ?? undefined : undefined}
          max={field.valueType === "number" ? field.max ?? undefined : undefined}
          minLength={field.valueType !== "number" ? field.min ?? undefined : undefined}
          maxLength={field.valueType !== "number" ? field.max ?? undefined : undefined}
          required={field.required}
          autoComplete={field.sensitive ? "new-password" : "off"}
          spellCheck={false}
        />
        {pathField ? (
          <button type="button" className="btn btn--ghost btn--sm" onClick={onPickPath}>
            {tr("ext.plugins.configChoosePath")}
          </button>
        ) : null}
      </div>
    );
  };

  const arrayValue = Array.isArray(value) ? value : [];
  return (
    <label className="field">
      <span>
        {label}
        {field.required ? " *" : ""}
        <span className="ext-plugin-config__type"> · {typeLabel}</span>
        {field.sensitive ? (
          <span className="ext-plugin-config__sensitive"> · {tr("ext.plugins.configSensitive")}</span>
        ) : null}
      </span>
      {field.description ? <span className="ext-field-hint">{field.description}</span> : null}
      {field.multiple && field.valueType === "select" && enumValues.length > 0 ? (
        renderSelect(arrayValue, true)
      ) : field.multiple ? (
        <div className="ext-plugin-config__multi">
          {arrayValue.map((item, index) => (
            <div className="ext-plugin-config__multi-row" key={`${field.name}:${index}`}>
              {renderScalar(item, (next) => {
                const nextValues = [...arrayValue];
                nextValues[index] = next;
                onChange(nextValues);
              })}
              <button
                type="button"
                className="btn btn--ghost btn--sm"
                onClick={() => onChange(arrayValue.filter((_, itemIndex) => itemIndex !== index))}
                aria-label={tr("ext.plugins.configRemoveValue")}
              >
                −
              </button>
            </div>
          ))}
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={() => onChange([...arrayValue, field.valueType === "boolean" ? false : ""])}
          >
            {tr("ext.plugins.configAddValue")}
          </button>
        </div>
      ) : (
        renderScalar(value, onChange)
      )}
      {field.min != null || field.max != null ? (
        <span className="ext-field-hint">
          {tr("ext.plugins.configBounds", {
            min: field.min == null ? "−∞" : field.min,
            max: field.max == null ? "+∞" : field.max,
          })}
        </span>
      ) : null}
    </label>
  );
}

/** Parse KEY=value lines into a map. Skips blanks and `#` comments. */
function parseEnvLines(raw: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of raw.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const eq = trimmed.indexOf("=");
    if (eq <= 0) continue;
    const key = trimmed.slice(0, eq).trim();
    if (!key) continue;
    let val = trimmed.slice(eq + 1).trim();
    if (
      (val.startsWith('"') && val.endsWith('"')) ||
      (val.startsWith("'") && val.endsWith("'"))
    ) {
      val = val.slice(1, -1);
    }
    out[key] = val;
  }
  return out;
}

function ExtensionToggle({
  checked,
  disabled,
  label,
  onChange,
}: {
  checked: boolean;
  disabled?: boolean;
  label: string;
  onChange: (next: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      title={label}
      disabled={disabled}
      className={"ext-switch" + (checked ? " is-on" : "")}
      onClick={() => onChange(!checked)}
    >
      <span className="ext-switch__thumb" aria-hidden />
    </button>
  );
}

function looksLikePath(target: string): boolean {
  const t = target.trim();
  if (!t) return false;
  if (t.startsWith("/") || /^[A-Za-z]:[\\/]/.test(t)) return true;
  if (t.startsWith("~")) return true;
  if (/\s/.test(t) || t.startsWith("http://") || t.startsWith("https://")) {
    return false;
  }
  return t.includes("/") || t.includes("\\");
}

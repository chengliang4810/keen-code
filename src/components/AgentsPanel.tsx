/** 设置 → 子智能体：查看内置定义并管理 KeenCode 全局定义。 */

import { useCallback, useEffect, useMemo, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import { GlassModal } from "@/components/GlassModal";
import { IconFolder, IconPlus, IconTrash, IconUser } from "@/components/icons";
import { shortPathLabel } from "@/lib/extensionsUi";

export interface AgentsPanelProps {
  locale: Locale;
}

/** 工具访问模式 → agent_create 的 tools 参数；null 表示继承主智能体全部工具。 */
export function agentToolsPayload(
  mode: "all" | "specific",
  selected: ReadonlySet<string>,
): string[] | null {
  return mode === "all" ? null : [...selected];
}

/** 展示并管理所有项目共享的子智能体。 */
export function AgentsPanel({ locale }: AgentsPanelProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [agents, setAgents] = useState<api.AgentDto[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [removeTarget, setRemoveTarget] = useState<api.AgentDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [prompt, setPrompt] = useState("");
  const [toolsMode, setToolsMode] = useState<"all" | "specific">("all");
  const [selectedTools, setSelectedTools] = useState<Set<string>>(new Set());
  const [catalog, setCatalog] = useState<string[]>([]);
  const [maxTurns, setMaxTurns] = useState("20");
  /** 模型覆盖下拉的分组选项：providerId → 模型列表。 */
  const [providerGroups, setProviderGroups] = useState<
    Array<{ providerId: string; providerLabel: string; models: string[] }>
  >([]);

  /** 刷新全局与内置子智能体。 */
  const refresh = useCallback(async () => {
    if (!api.isTauri()) {
      setAgents([]);
      setError(tr("ext.needTauri"));
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const result = await api.agentsList();
      setAgents(result.agents);
    } catch (cause) {
      setAgents([]);
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }, [tr]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  /** 加载模型覆盖下拉的候选分组。 */
  useEffect(() => {
    if (!api.isTauri()) return;
    void api
      .providersList()
      .then((list) => {
        setProviderGroups(
          list.providers
            .filter((provider) => provider.models.length > 0)
            .map((provider) => ({
              providerId: provider.id,
              providerLabel: provider.name.trim() || provider.id,
              models: provider.models,
            })),
        );
      })
      .catch(() => setProviderGroups([]));
  }, []);

  /** 保存子智能体的模型覆盖；空值清除覆盖、跟随会话 Provider。 */
  const saveAgentModel = async (agent: api.AgentDto, value: string) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.agentUpdate(agent.name, value ? value : null);
      await refresh();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  /** 打开创建弹窗并加载可勾选工具目录。 */
  const openCreate = async () => {
    setCreateOpen(true);
    if (catalog.length === 0 && api.isTauri()) {
      try {
        const result = await api.agentsToolCatalog();
        setCatalog(result.tools);
      } catch {
        setCatalog([]);
      }
    }
  };

  /** 清空创建表单并关闭弹窗。 */
  const closeCreate = () => {
    if (busy) return;
    setCreateOpen(false);
    setName("");
    setDescription("");
    setPrompt("");
    setToolsMode("all");
    setSelectedTools(new Set());
    setMaxTurns("20");
  };

  /** 将可视化表单保存为全局 `~/.keencode/agents/{name}.md`。 */
  const createAgent = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.agentCreate({
        name: name.trim(),
        description: description.trim(),
        prompt: prompt.trim(),
        tools: agentToolsPayload(toolsMode, selectedTools),
        maxTurns: maxTurns.trim() ? Number(maxTurns) : null,
      });
      setCreateOpen(false);
      setName("");
      setDescription("");
      setPrompt("");
      setToolsMode("all");
      setSelectedTools(new Set());
      setMaxTurns("20");
      await refresh();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  /** 切换指定模式下单个工具的勾选状态。 */
  const toggleTool = (tool: string) => {
    setSelectedTools((current) => {
      const next = new Set(current);
      if (next.has(tool)) next.delete(tool);
      else next.add(tool);
      return next;
    });
  };

  /** 删除已经确认的全局子智能体。 */
  const removeAgent = async () => {
    const target = removeTarget;
    if (!target || busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.agentRemove(target.name);
      setRemoveTarget(null);
      await refresh();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  const canCreate = Boolean(
    name.trim() &&
      description.trim() &&
      prompt.trim() &&
      (toolsMode === "all" || selectedTools.size > 0) &&
      (!maxTurns.trim() || (Number.isInteger(Number(maxTurns)) && Number(maxTurns) > 0)),
  );

  return (
    <div className="ext-panel" data-testid="agents-panel">
      <p className="settings-page__lead">{tr("agents.lead")}</p>
      {error ? <p className="ext-alert ext-alert--error" role="alert">{error}</p> : null}
      <h2 className="settings-page__h2" id="settings-anchor-agents">
        <IconUser size={15} />
        {tr("agents.title")}
        {!loading ? <span className="ext-count">{agents.length}</span> : null}
        <button
          type="button"
          className="btn btn--solid settings-page__h2-action"
          disabled={busy || !api.isTauri()}
          onClick={() => void openCreate()}
        >
          <IconPlus size={14} />
          <span>{tr("agents.add")}</span>
        </button>
      </h2>
      <div className="settings-card ext-card">
        {loading ? <p className="ext-empty">{tr("agents.loading")}</p> : null}
        {!loading && agents.length === 0 ? <p className="ext-empty">{tr("agents.empty")}</p> : null}
        {!loading && agents.length > 0 ? (
          <ul className="ext-list">
            {agents.map((agent) => (
              <li key={`${agent.source}:${agent.name}`} className="ext-item">
                <div className="ext-item__head">
                  <strong className="ext-item__name">{agent.name}</strong>
                  <span className={`ext-badge ext-badge--${agent.source === "global" ? "user" : "muted"}`}>
                    {agent.source === "global"
                      ? tr("agents.source.global")
                      : agent.source === "plugin"
                        ? tr("agents.source.plugin")
                        : tr("agents.source.builtin")}
                  </span>
                </div>
                <p className="ext-item__desc">{agent.description}</p>
                {agent.path ? (
                  <div className="ext-item__meta">
                    <button type="button" className="ext-path-btn" title={agent.path} onClick={() => void api.pathReveal(agent.path!)}>
                      <IconFolder size={13} />
                      <span>{shortPathLabel(agent.path, 48)}</span>
                    </button>
                  </div>
                ) : null}
                {agent.source === "global" ? (
                  <div className="ext-item__meta ext-item__meta--model">
                    <span className="ext-agent-model__label">
                      {tr("agents.model")}
                    </span>
                    <select
                      className="ext-agent-model__select"
                      value={agent.model ?? ""}
                      disabled={busy}
                      aria-label={tr("agents.model")}
                      onChange={(event) =>
                        void saveAgentModel(agent, event.target.value)
                      }
                    >
                      <option value="">
                        {tr("agents.model.followSession")}
                      </option>
                      {providerGroups.map((group) => (
                        <optgroup key={group.providerId} label={group.providerLabel}>
                          {group.models.map((model) => (
                            <option key={model} value={`${group.providerId}::${model}`}>
                              {model}
                            </option>
                          ))}
                        </optgroup>
                      ))}
                      {agent.model &&
                      !providerGroups.some((group) =>
                        group.models.some(
                          (model) => `${group.providerId}::${model}` === agent.model,
                        ),
                      ) ? (
                        <option value={agent.model} disabled>
                          {agent.model}
                        </option>
                      ) : null}
                    </select>
                  </div>
                ) : null}
                {agent.source === "global" ? (
                  <div className="ext-item__actions">
                    <button type="button" className="btn btn--ghost btn--sm ext-item__danger" disabled={busy} onClick={() => setRemoveTarget(agent)}>
                      <IconTrash size={13} />
                      <span>{tr("agents.remove")}</span>
                    </button>
                  </div>
                ) : null}
              </li>
            ))}
          </ul>
        ) : null}
      </div>

      <GlassModal open={createOpen} title={tr("agents.addTitle")} onClose={closeCreate}>
        <div className="ext-modal-form">
          <label className="ext-plugin-install__label" htmlFor="agent-name">{tr("agents.name")}</label>
          <input id="agent-name" className="settings-input" value={name} placeholder="code-reviewer" onChange={(event) => setName(event.target.value)} />
          <label className="ext-plugin-install__label" htmlFor="agent-description">{tr("agents.description")}</label>
          <input id="agent-description" className="settings-input" value={description} onChange={(event) => setDescription(event.target.value)} />
          <label className="ext-plugin-install__label" htmlFor="agent-prompt">{tr("agents.prompt")}</label>
          <textarea id="agent-prompt" className="settings-input ext-agent-textarea" rows={7} value={prompt} onChange={(event) => setPrompt(event.target.value)} />
          <span className="ext-plugin-install__label">{tr("agents.tools")}</span>
          <div className="ext-tools-mode" role="radiogroup" aria-label={tr("agents.tools")}>
            <label className="ext-tools-mode__option">
              <input
                type="radio"
                name="agent-tools-mode"
                checked={toolsMode === "all"}
                onChange={() => setToolsMode("all")}
              />
              <span className="ext-tools-mode__text">
                <span>{tr("agents.tools.all")}</span>
                <span className="ext-tools-mode__hint">{tr("agents.tools.allHint")}</span>
              </span>
            </label>
            <label className="ext-tools-mode__option">
              <input
                type="radio"
                name="agent-tools-mode"
                checked={toolsMode === "specific"}
                onChange={() => setToolsMode("specific")}
              />
              <span className="ext-tools-mode__text">{tr("agents.tools.specific")}</span>
            </label>
          </div>
          {toolsMode === "specific" ? (
            <div className="ext-tools-picker" role="list" aria-label={tr("agents.tools.specific")}>
              {catalog.map((tool) => (
                <label className="ext-tools-picker__row" role="listitem" key={tool}>
                  <input
                    type="checkbox"
                    checked={selectedTools.has(tool)}
                    onChange={() => toggleTool(tool)}
                  />
                  <span>{tool}</span>
                </label>
              ))}
              {catalog.length === 0 ? (
                <p className="ext-empty">{tr("agents.tools.empty")}</p>
              ) : null}
            </div>
          ) : null}
          <label className="ext-plugin-install__label" htmlFor="agent-max-turns">{tr("agents.maxTurns")}</label>
          <input id="agent-max-turns" className="settings-input" type="number" min={1} value={maxTurns} onChange={(event) => setMaxTurns(event.target.value)} />
          <div className="ext-item__actions">
            <button type="button" className="btn btn--ghost" disabled={busy} onClick={closeCreate}>{tr("common.cancel")}</button>
            <button type="button" className="btn btn--solid" disabled={!canCreate || busy} onClick={() => void createAgent()}>{busy ? tr("agents.creating") : tr("agents.create")}</button>
          </div>
        </div>
      </GlassModal>

      <GlassModal open={!!removeTarget} title={tr("agents.removeTitle")} onClose={() => !busy && setRemoveTarget(null)}>
        <p>{tr("agents.removeConfirm", { name: removeTarget?.name ?? "" })}</p>
        <div className="ext-item__actions">
          <button type="button" className="btn btn--ghost" disabled={busy} onClick={() => setRemoveTarget(null)}>{tr("common.cancel")}</button>
          <button type="button" className="btn btn--solid" disabled={busy} onClick={() => void removeAgent()}>{tr("agents.remove")}</button>
        </div>
      </GlassModal>
    </div>
  );
}

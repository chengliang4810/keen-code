import { Input } from "@appica/ui-react/input";
import { Textarea } from "@appica/ui-react/textarea";
import { Button } from "@/components/ui/button";
import { Badge } from "@appica/ui-react/badge";
import { Alert, AlertDescription } from "@appica/ui-react/alert";
import { Card } from "@appica/ui-react/card";
import { NumberField } from "@appica/ui-react/number-field";
import { Field, FieldLabel } from "@appica/ui-react/field";
/** 设置 → 子智能体：查看内置定义并管理 KeenCode 全局定义。 */

import { useCallback, useEffect, useMemo, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import { localizeUiError } from "@/lib/session";
import { GlassModal } from "@/components/GlassModal";
import {
  IconFolder,
  IconPlus,
  IconSubagent,
  IconTrash,
  IconUser,
} from "@/components/icons";
import { SkeletonList } from "@/components/Skeleton";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@appica/ui-react/select";
import { Checkbox } from "@appica/ui-react/checkbox";
import { Switch } from "@/components/ui/switch";
import { shortPathLabel } from "@/lib/extensionsUi";
import { ProviderModelMenu } from "@/components/ProviderModelMenu";

export interface AgentsPanelProps {
  locale: Locale;
  /** 当前工作台项目路径；为空时只查询全局与内置子智能体。 */
  projectPath?: string | null;
}

/** 工具访问模式 → agent_create 的 tools 参数；null 表示继承主智能体全部工具。 */
export function agentToolsPayload(
  mode: "all" | "specific",
  selected: ReadonlySet<string>,
): string[] | null {
  return mode === "all" ? null : [...selected];
}

type AgentProviderGroup = {
  providerId: string;
  providerLabel: string;
  models: string[];
};

/** 设置页只接受 `providerId::model`，定义中的其他模型值不作为覆盖展示。 */
function normalizeAgentModelReference(value: string): string | null {
  const trimmed = value.trim();
  const separator = trimmed.indexOf("::");
  if (separator <= 0 || trimmed.slice(separator + 2).trim().length === 0) return null;
  if (trimmed.slice(separator + 2).includes("::")) return null;
  if ([...trimmed].some((character) => /[\u0000-\u001f\u007f]/.test(character))) return null;
  const provider = trimmed.slice(0, separator).trim();
  const model = trimmed.slice(separator + 2).trim();
  return provider && model ? `${provider}::${model}` : null;
}

function findAgentModelOption(
  value: string,
  providerGroups: ReadonlyArray<AgentProviderGroup>,
): { providerId: string; model: string } | null {
  for (const group of providerGroups) {
    for (const model of group.models) {
      if (value === `${group.providerId}::${model}`) {
        return { providerId: group.providerId, model };
      }
    }
  }
  return null;
}

/** 详情弹窗的只读展示；抽出为纯组件便于静态渲染测试。 */
export function AgentDetailView({
  locale,
  detail,
}: {
  locale: Locale;
  detail: api.AgentDetailDto;
}) {
  const tr = createT(locale);
  const model = detail.model ? normalizeAgentModelReference(detail.model) : null;
  return (
    <div className="ext-agent-detail" data-testid="agent-detail">
      <div className="ext-item__head">
        <strong className="ext-item__name">{detail.name}</strong>
        <Badge size="xs" variant={detail.source === "global" ? "primary-outline" : "soft"}>
          {detail.source === "global"
            ? tr("agents.source.global")
            : detail.source === "plugin"
              ? tr("agents.source.plugin")
              : tr("agents.source.builtin")}
        </Badge>
      </div>
      <p className="ext-item__desc">{detail.description}</p>
      {model || detail.maxTurns || detail.path ? (
        <div className="ext-item__meta">
          {model ? <span>{tr("agents.model")}: {model}</span> : null}
          {detail.maxTurns ? <span>{tr("agents.maxTurns")}: {detail.maxTurns}</span> : null}
          {detail.path ? <span title={detail.path}>{shortPathLabel(detail.path, 48)}</span> : null}
        </div>
      ) : null}
      <span className="ext-plugin-install__label">{tr("agents.tools")}</span>
      <p className="ext-agent-detail__value">
        {detail.tools === null
          ? tr("agents.detail.toolsInherit")
          : detail.tools.length > 0
            ? detail.tools.join(", ")
            : tr("agents.detail.toolsNone")}
      </p>
      {detail.disallowedTools.length > 0 ? (
        <>
          <span className="ext-plugin-install__label">{tr("agents.detail.disallowed")}</span>
          <p className="ext-agent-detail__value">{detail.disallowedTools.join(", ")}</p>
        </>
      ) : null}
      {detail.allowedWriteDirs.length > 0 ? (
        <>
          <span className="ext-plugin-install__label">{tr("agents.detail.sandboxDirs")}</span>
          <p className="ext-agent-detail__value">{detail.allowedWriteDirs.join(", ")}</p>
        </>
      ) : null}
      <span className="ext-plugin-install__label">{tr("agents.prompt")}</span>
      <pre id="agent-detail-prompt" className="ext-agent-detail__prompt" data-testid="agent-detail-prompt">
        {detail.systemPrompt}
      </pre>
    </div>
  );
}

/** 创建表单的模型选择：空值跟随会话 Provider，否则 providerId::model。 */
export function AgentModelPicker({
  locale,
  value,
  providerGroups,
  onChange,
}: {
  locale: Locale;
  value: string;
  providerGroups: ReadonlyArray<AgentProviderGroup>;
  onChange: (value: string) => void;
}) {
  const tr = createT(locale);
  return (
    <>
      <label className="ext-plugin-install__label" htmlFor="agent-model">{tr("agents.model.assign")}</label>
      <AgentModelSelect
        locale={locale}
        value={value}
        providerGroups={providerGroups}
        id="agent-model"
        className="settings-input"
        align="start"
        label={tr("agents.model.assign")}
        onSelect={onChange}
      />
    </>
  );
}

/** 列表行的模型选择器：供应商子菜单 + 继承默认项；空值跟随会话 Provider。 */
export function AgentModelSelect({
  locale,
  value,
  providerGroups,
  disabled,
  id,
  className = "ext-agent-model__trigger",
  align = "end",
  label: accessibleLabel,
  onSelect,
}: {
  locale: Locale;
  value: string | null;
  providerGroups: ReadonlyArray<AgentProviderGroup>;
  disabled?: boolean;
  id?: string;
  className?: string;
  align?: "start" | "center" | "end";
  label?: string;
  onSelect: (value: string) => void;
}) {
  const tr = createT(locale);
  const selectedModel = value ? findAgentModelOption(value, providerGroups) : null;
  const label = selectedModel?.model ?? tr("agents.model.followSession");
  return (
    <ProviderModelMenu
      groups={providerGroups.map((group) => ({
        id: group.providerId,
        label: group.providerLabel,
        models: group.models.map((model) => ({ id: model, label: model })),
      }))}
      selectedProviderId={selectedModel?.providerId}
      selectedModelId={selectedModel?.model}
      triggerId={id}
      triggerClassName={className}
      triggerContent={<span className="ext-agent-model__trigger-text">{label}</span>}
      triggerLabel={accessibleLabel ?? tr("agents.model")}
      triggerTitle={selectedModel ? value! : tr("agents.model.followSession")}
      disabled={disabled}
      align={align}
      contentClassName="ext-agent-model__menu w-56"
      modelContentClassName="ext-agent-model__menu w-56"
      emptyOption={{
        label: tr("agents.model.followSession"),
        selected: !selectedModel,
        onSelect: () => onSelect(""),
      }}
      onModelSelect={(providerId, modelId) => onSelect(`${providerId}::${modelId}`)}
    />
  );
}

/** 展示并管理当前项目可用的全局、项目与插件子智能体。 */
export function AgentsPanel({ locale, projectPath = null }: AgentsPanelProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [agents, setAgents] = useState<api.AgentDto[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [removeTarget, setRemoveTarget] = useState<api.AgentDto | null>(null);
  const [detail, setDetail] = useState<api.AgentDetailDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [prompt, setPrompt] = useState("");
  const [toolsMode, setToolsMode] = useState<"all" | "specific">("all");
  const [injectAgentsMd, setInjectAgentsMd] = useState(true);
  const [selectedTools, setSelectedTools] = useState<Set<string>>(new Set());
  const [catalog, setCatalog] = useState<string[]>([]);
  const [maxTurns, setMaxTurns] = useState("");
  /** 创建表单的模型覆盖：空串跟随会话 Provider，否则 providerId::model。 */
  const [createModel, setCreateModel] = useState("");
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
      const result = await api.agentsList(projectPath?.trim() || null);
      setAgents(result.agents);
    } catch (cause) {
      setAgents([]);
      setError(localizeUiError(cause, locale));
    } finally {
      setLoading(false);
    }
  }, [locale, projectPath, tr]);

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
      setError(localizeUiError(cause, locale));
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
    setInjectAgentsMd(true);
    setSelectedTools(new Set());
    setMaxTurns("");
    setCreateModel("");
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
        model: createModel || null,
      });
      setCreateOpen(false);
      setName("");
      setDescription("");
      setPrompt("");
      setToolsMode("all");
      setInjectAgentsMd(true);
      setSelectedTools(new Set());
      setMaxTurns("");
      setCreateModel("");
      await refresh();
    } catch (cause) {
      setError(localizeUiError(cause, locale));
    } finally {
      setBusy(false);
    }
  };

  /** 设置指定模式下单个工具的勾选状态。 */
  const setToolChecked = (tool: string, checked: boolean) => {
    setSelectedTools((current) => {
      const next = new Set(current);
      if (checked) next.add(tool);
      else next.delete(tool);
      return next;
    });
  };

  /** 加载并打开单个子智能体的定义详情。 */
  const openDetail = async (agent: api.AgentDto) => {
    if (!api.isTauri()) return;
    try {
      setDetail(await api.agentDetail(agent.name, projectPath?.trim() || null));
    } catch (cause) {
      setError(localizeUiError(cause, locale));
    }
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
      setError(localizeUiError(cause, locale));
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
      {error ? <Alert variant="error"><AlertDescription>{error}</AlertDescription></Alert> : null}
      <h2 className="settings-page__h2" id="settings-anchor-agents">
        <IconUser size={15} />
        {tr("agents.title")}
        {!loading ? <span className="ext-count">{agents.length}</span> : null}
        <Button
          type="button"
          variant="primary" className="settings-page__h2-action"
          disabled={busy || !api.isTauri()}
          onClick={() => void openCreate()}
        >
          <IconPlus size={14} />
          <span>{tr("agents.add")}</span>
        </Button>
      </h2>
      <Card inset={false} className="ext-card ext-agent-card">
        {loading && <SkeletonList rows={3} label={tr("agents.loading")} />}
        {!loading && agents.length === 0 ? <p className="ext-empty">{tr("agents.empty")}</p> : null}
        {!loading && agents.length > 0 ? (
          <ul className="ext-list ext-agent-list">
            {agents.map((agent) => (
              <li key={`${agent.source}:${agent.name}`} className="ext-item ext-agent-row">
                <span className="ext-agent-row__icon" aria-hidden="true">
                  <IconSubagent size={20} />
                </span>
                <div className="ext-agent-row__content">
                  <div className="ext-agent-row__heading">
                    <Button
                      type="button"
                      variant="ghost"
                      size="md"
                      className="ext-item__head-btn"
                      title={tr("agents.detail.view")}
                      onClick={() => void openDetail(agent)}
                    >
                      <strong className="ext-item__name">{agent.name}</strong>
                    </Button>
                    <Badge size="xs" variant={agent.source === "global" ? "primary-outline" : "soft"}>
                      {agent.source === "global"
                        ? tr("agents.source.global")
                        : agent.source === "plugin"
                          ? tr("agents.source.plugin")
                          : tr("agents.source.builtin")}
                    </Badge>
                  </div>
                  <p className="ext-item__desc">{agent.description}</p>
                  {agent.path ? (
                    <Button type="button" variant="ghost" size="md" className="ext-path-btn" title={agent.path} onClick={() => void api.pathReveal(agent.path!)}>
                      <IconFolder size={13} />
                      <span>{shortPathLabel(agent.path, 48)}</span>
                    </Button>
                  ) : null}
                </div>
                <div className="ext-agent-row__controls">
                  {agent.source === "global" || agent.source === "builtin" ? (
                    <div className="ext-item__model">
                      <AgentModelSelect
                        locale={locale}
                        value={agent.model}
                        providerGroups={providerGroups}
                        disabled={busy}
                        onSelect={(next) => void saveAgentModel(agent, next)}
                      />
                    </div>
                  ) : null}
                  {agent.source === "global" ? (
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-md"
                      title={tr("agents.remove")}
                      aria-label={tr("agents.remove")}
                      disabled={busy}
                      onClick={() => setRemoveTarget(agent)}
                    >
                      <IconTrash size={15} />
                    </Button>
                  ) : null}
                </div>
              </li>
            ))}
          </ul>
        ) : null}
      </Card>

      <GlassModal
        open={createOpen}
        title={tr("agents.addTitle")}
        className="ext-agent-create-modal"
        bodyClassName="ext-agent-create-modal__body"
        wrapBody
        onClose={closeCreate}
        footer={
          <>
            <Button type="button" variant="ghost" disabled={busy} onClick={closeCreate}>{tr("common.cancel")}</Button>
            <Button type="button" variant="primary" disabled={!canCreate || busy} onClick={() => void createAgent()}>{busy ? tr("agents.creating") : tr("agents.create")}</Button>
          </>
        }
      >
        <div className="ext-agent-create">
          <Field className="ext-agent-create__name">
            <FieldLabel htmlFor="agent-name">{tr("agents.name")}</FieldLabel>
            <Input data-modal-autofocus id="agent-name" value={name} placeholder="code-reviewer" onChange={(event) => setName(event.target.value)} />
          </Field>
          <div className="ext-agent-create__model">
            <AgentModelPicker locale={locale} value={createModel} providerGroups={providerGroups} onChange={setCreateModel} />
          </div>
          <Field className="ext-agent-create__turns">
            <FieldLabel htmlFor="agent-max-turns">{tr("agents.maxTurns")}</FieldLabel>
            <NumberField id="agent-max-turns" min={1} value={maxTurns ? Number(maxTurns) : null} onValueChange={(value) => setMaxTurns(value == null ? "" : String(value))} />
          </Field>
          <Field className="ext-agent-create__description">
            <FieldLabel htmlFor="agent-description">{tr("agents.description")}</FieldLabel>
            <Input id="agent-description" value={description} onChange={(event) => setDescription(event.target.value)} />
          </Field>
          <section className="ext-agent-create__tools" aria-labelledby="agent-tools-label">
            <span className="ext-plugin-install__label" id="agent-tools-label">{tr("agents.tools")}</span>
            <Select
              value={toolsMode}
              onValueChange={(value) => {
                if (typeof value === "string" && (value === "all" || value === "specific")) {
                  setToolsMode(value);
                }
              }}
            >
              <SelectTrigger className="ext-agent-tools-select" aria-label={tr("agents.tools")}>
                <SelectValue>
                  {() => toolsMode === "all" ? tr("agents.tools.all") : tr("agents.tools.specific")}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  <SelectItem value="all">{tr("agents.tools.all")}</SelectItem>
                  <SelectItem value="specific">{tr("agents.tools.specific")}</SelectItem>
                </SelectGroup>
              </SelectContent>
            </Select>
            <span className="ext-tools-mode__hint">
              {toolsMode === "all" ? tr("agents.tools.allHint") : tr("agents.tools.specificHint")}
            </span>
            {toolsMode === "specific" ? (
              <div className="ext-tools-picker" role="list" aria-label={tr("agents.tools.specific")}>
                {catalog.map((tool, index) => {
                  const toolId = `agent-tool-${index}`;
                  return (
                    <div className="ext-tools-picker__row" role="listitem" key={tool}>
                      <Checkbox id={toolId} checked={selectedTools.has(tool)} aria-label={tool} onCheckedChange={(checked) => setToolChecked(tool, checked === true)} />
                      <label htmlFor={toolId}>{tool}</label>
                    </div>
                  );
                })}
                {catalog.length === 0 ? <p className="ext-empty">{tr("agents.tools.empty")}</p> : null}
              </div>
            ) : null}
          </section>
          <Field className="ext-agent-create__prompt">
            <FieldLabel htmlFor="agent-prompt">{tr("agents.prompt")}</FieldLabel>
            <Textarea id="agent-prompt" className="ext-agent-textarea" rows={7} value={prompt} onChange={(event) => setPrompt(event.target.value)} />
          </Field>
          <div className="ext-agent-create__agents-md">
            <div>
              <strong>{tr("agents.injectAgentsMd")}</strong>
              <span>{tr("agents.injectAgentsMdHint")}</span>
            </div>
            <Switch
              checked={injectAgentsMd}
              aria-label={tr("agents.injectAgentsMd")}
              onCheckedChange={(checked) => setInjectAgentsMd(checked === true)}
            />
          </div>
        </div>
      </GlassModal>

      <GlassModal open={!!detail} title={tr("agents.detailTitle")} onClose={() => setDetail(null)}>
        {detail ? <AgentDetailView locale={locale} detail={detail} /> : null}
      </GlassModal>

      <GlassModal open={!!removeTarget} title={tr("agents.removeTitle")} onClose={() => !busy && setRemoveTarget(null)}>
        <p>{tr("agents.removeConfirm", { name: removeTarget?.name ?? "" })}</p>
        <div className="ext-item__actions">
          <Button type="button" variant="ghost" disabled={busy} onClick={() => setRemoveTarget(null)}>{tr("common.cancel")}</Button>
          <Button type="button" variant="primary" disabled={busy} onClick={() => void removeAgent()}>{tr("agents.remove")}</Button>
        </div>
      </GlassModal>
    </div>
  );
}

import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { Button } from "@/components/ui/button";
/** 设置 → 子智能体：查看内置定义并管理 KeenCode 全局定义。 */

import { useCallback, useEffect, useMemo, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import { GlassModal } from "@/components/GlassModal";
import {
  IconCheck,
  IconChevronDown,
  IconFolder,
  IconPlus,
  IconTrash,
  IconUser,
} from "@/components/icons";
import { SkeletonList } from "@/components/Skeleton";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuPortal,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Checkbox } from "@/components/ui/checkbox";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
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

type AgentProviderGroup = {
  providerId: string;
  providerLabel: string;
  models: string[];
};

/** Radix Select reserves an empty string, so the session-following option uses a stable sentinel. */
export const AGENT_MODEL_SESSION_VALUE = "agent-model:session";
const AGENT_MODEL_OPTION_PREFIX = "agent-model:option:";

function encodeAgentModelOption(providerId: string, model: string): string {
  return `${AGENT_MODEL_OPTION_PREFIX}${encodeURIComponent(providerId)}:${encodeURIComponent(model)}`;
}

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
): { providerId: string; model: string; selectValue: string } | null {
  for (const group of providerGroups) {
    for (const model of group.models) {
      const selectValue = encodeAgentModelOption(group.providerId, model);
      if (value === `${group.providerId}::${model}`) {
        return { providerId: group.providerId, model, selectValue };
      }
    }
  }
  return null;
}

/** Encode a persisted model override only when it belongs to the current provider/model catalog. */
export function encodeAgentModelSelectValue(
  value: string,
  providerGroups: ReadonlyArray<AgentProviderGroup>,
): string {
  if (!value) return AGENT_MODEL_SESSION_VALUE;
  return findAgentModelOption(value, providerGroups)?.selectValue ?? AGENT_MODEL_SESSION_VALUE;
}

/** Decode a Radix value and reject values that are not present in the current catalog. */
export function decodeAgentModelSelectValue(
  value: string,
  providerGroups: ReadonlyArray<AgentProviderGroup>,
): string | null {
  if (value === AGENT_MODEL_SESSION_VALUE) return "";
  for (const group of providerGroups) {
    for (const model of group.models) {
      if (value === encodeAgentModelOption(group.providerId, model)) {
        return `${group.providerId}::${model}`;
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
        <span className={`ext-badge ext-badge--${detail.source === "global" ? "user" : "muted"}`}>
          {detail.source === "global"
            ? tr("agents.source.global")
            : detail.source === "plugin"
              ? tr("agents.source.plugin")
              : tr("agents.source.builtin")}
        </span>
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
  const selectValue = encodeAgentModelSelectValue(value, providerGroups);
  return (
    <>
      <Label className="ext-plugin-install__label" htmlFor="agent-model">{tr("agents.model.assign")}</Label>
      <Select
        value={selectValue}
        onValueChange={(nextValue) => {
          const decodedValue = decodeAgentModelSelectValue(nextValue, providerGroups);
          if (decodedValue !== null) onChange(decodedValue);
        }}
      >
        <SelectTrigger
          id="agent-model"
          className="settings-input"
          aria-label={tr("agents.model.assign")}
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectGroup>
            <SelectItem value={AGENT_MODEL_SESSION_VALUE}>
              {tr("agents.model.followSession")}
            </SelectItem>
          </SelectGroup>
          {providerGroups.map((group) => (
            <SelectGroup key={group.providerId}>
              <SelectLabel>{group.providerLabel}</SelectLabel>
              {group.models.map((model) => (
                <SelectItem
                  key={`${group.providerId}::${model}`}
                  value={encodeAgentModelOption(group.providerId, model)}
                >
                  {model}
                </SelectItem>
              ))}
            </SelectGroup>
          ))}
        </SelectContent>
      </Select>
    </>
  );
}

/** 列表行的模型选择器：供应商子菜单 + 继承默认项；空值跟随会话 Provider。 */
export function AgentModelSelect({
  locale,
  value,
  providerGroups,
  disabled,
  onSelect,
}: {
  locale: Locale;
  value: string | null;
  providerGroups: ReadonlyArray<AgentProviderGroup>;
  disabled?: boolean;
  onSelect: (value: string) => void;
}) {
  const tr = createT(locale);
  const selectedModel = value ? findAgentModelOption(value, providerGroups) : null;
  const label = selectedModel?.model ?? tr("agents.model.followSession");
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          className="ext-agent-model__trigger"
          disabled={disabled}
          aria-label={tr("agents.model")}
          title={selectedModel ? value! : tr("agents.model.followSession")}
        >
          <span className="ext-agent-model__trigger-text">{label}</span>
          <IconChevronDown size={12} className="chevron" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="end"
        sideOffset={6}
        className="ext-agent-model__menu w-56"
      >
        <DropdownMenuGroup>
          <DropdownMenuItem onSelect={() => onSelect("")}>
            <span className="truncate">{tr("agents.model.followSession")}</span>
            {!selectedModel ? (
              <span className="ml-auto" aria-hidden>
                <IconCheck size={16} />
              </span>
            ) : null}
          </DropdownMenuItem>
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuGroup>
          {providerGroups.map((group) => (
            <DropdownMenuSub key={group.providerId}>
              <DropdownMenuSubTrigger>
                <span className="truncate">{group.providerLabel}</span>
              </DropdownMenuSubTrigger>
              <DropdownMenuPortal>
                <DropdownMenuSubContent className="ext-agent-model__menu w-56">
                  <DropdownMenuGroup>
                    {group.models.map((model) => {
                      const selected = selectedModel?.providerId === group.providerId
                        && selectedModel.model === model;
                      return (
                        <DropdownMenuItem
                          key={model}
                          onSelect={() => onSelect(`${group.providerId}::${model}`)}
                        >
                          <span className="truncate">{model}</span>
                          {selected ? (
                            <span className="ml-auto" aria-hidden>
                              <IconCheck size={16} />
                            </span>
                          ) : null}
                        </DropdownMenuItem>
                      );
                    })}
                  </DropdownMenuGroup>
                </DropdownMenuSubContent>
              </DropdownMenuPortal>
            </DropdownMenuSub>
          ))}
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** 展示并管理所有项目共享的子智能体。 */
export function AgentsPanel({ locale }: AgentsPanelProps) {
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
  const [selectedTools, setSelectedTools] = useState<Set<string>>(new Set());
  const [catalog, setCatalog] = useState<string[]>([]);
  const [maxTurns, setMaxTurns] = useState("20");
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
      setSelectedTools(new Set());
      setMaxTurns("20");
      setCreateModel("");
      await refresh();
    } catch (cause) {
      setError(String(cause));
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
      setDetail(await api.agentDetail(agent.name));
    } catch (cause) {
      setError(String(cause));
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
        <Button
          type="button"
          className="btn btn--solid settings-page__h2-action"
          disabled={busy || !api.isTauri()}
          onClick={() => void openCreate()}
        >
          <IconPlus size={14} />
          <span>{tr("agents.add")}</span>
        </Button>
      </h2>
      <div className="settings-card ext-card">
        {loading && <SkeletonList rows={3} label={tr("agents.loading")} />}
        {!loading && agents.length === 0 ? <p className="ext-empty">{tr("agents.empty")}</p> : null}
        {!loading && agents.length > 0 ? (
          <ul className="ext-list">
            {agents.map((agent) => (
              <li key={`${agent.source}:${agent.name}`} className="ext-item">
                <div className="ext-item__body">
                  <div className="ext-item__main">
                    <Button
                      type="button"
                      className="ext-item__head-btn"
                      title={tr("agents.detail.view")}
                      onClick={() => void openDetail(agent)}
                    >
                      <strong className="ext-item__name">{agent.name}</strong>
                      <span className={`ext-badge ext-badge--${agent.source === "global" ? "user" : "muted"}`}>
                        {agent.source === "global"
                          ? tr("agents.source.global")
                          : agent.source === "plugin"
                            ? tr("agents.source.plugin")
                            : tr("agents.source.builtin")}
                      </span>
                    </Button>
                    <p className="ext-item__desc">{agent.description}</p>
                    {agent.path ? (
                      <div className="ext-item__meta">
                        <Button type="button" className="ext-path-btn" title={agent.path} onClick={() => void api.pathReveal(agent.path!)}>
                          <IconFolder size={13} />
                          <span>{shortPathLabel(agent.path, 48)}</span>
                        </Button>
                      </div>
                    ) : null}
                  </div>
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
                </div>
                {agent.source === "global" ? (
                  <div className="ext-item__actions">
                    <Button type="button" className="btn btn--ghost btn--sm ext-item__danger" disabled={busy} onClick={() => setRemoveTarget(agent)}>
                      <IconTrash size={13} />
                      <span>{tr("agents.remove")}</span>
                    </Button>
                  </div>
                ) : null}
              </li>
            ))}
          </ul>
        ) : null}
      </div>

      <GlassModal open={createOpen} title={tr("agents.addTitle")} onClose={closeCreate}>
        <div className="ext-modal-form">
          <Label className="ext-plugin-install__label" htmlFor="agent-name">{tr("agents.name")}</Label>
          <Input id="agent-name" className="settings-input" value={name} placeholder="code-reviewer" onChange={(event) => setName(event.target.value)} />
          <Label className="ext-plugin-install__label" htmlFor="agent-description">{tr("agents.description")}</Label>
          <Input id="agent-description" className="settings-input" value={description} onChange={(event) => setDescription(event.target.value)} />
          <Label className="ext-plugin-install__label" htmlFor="agent-prompt">{tr("agents.prompt")}</Label>
          <Textarea id="agent-prompt" className="settings-input ext-agent-textarea" rows={7} value={prompt} onChange={(event) => setPrompt(event.target.value)} />
          <span className="ext-plugin-install__label">{tr("agents.tools")}</span>
          <RadioGroup
            className="ext-tools-mode"
            name="agent-tools-mode"
            value={toolsMode}
            aria-label={tr("agents.tools")}
            onValueChange={(value) => {
              if (value === "all" || value === "specific") setToolsMode(value);
            }}
          >
            <div className="ext-tools-mode__option">
              <RadioGroupItem
                id="agent-tools-all"
                value="all"
                aria-label={tr("agents.tools.all")}
              />
              <Label className="ext-tools-mode__text" htmlFor="agent-tools-all">
                <span>{tr("agents.tools.all")}</span>
                <span className="ext-tools-mode__hint">{tr("agents.tools.allHint")}</span>
              </Label>
            </div>
            <div className="ext-tools-mode__option">
              <RadioGroupItem
                id="agent-tools-specific"
                value="specific"
                aria-label={tr("agents.tools.specific")}
              />
              <Label className="ext-tools-mode__text" htmlFor="agent-tools-specific">
                {tr("agents.tools.specific")}
              </Label>
            </div>
          </RadioGroup>
          {toolsMode === "specific" ? (
            <div className="ext-tools-picker" role="list" aria-label={tr("agents.tools.specific")}>
              {catalog.map((tool, index) => {
                const toolId = `agent-tool-${index}`;
                return (
                  <div className="ext-tools-picker__row" role="listitem" key={tool}>
                    <Checkbox
                      id={toolId}
                      checked={selectedTools.has(tool)}
                      aria-label={tool}
                      onCheckedChange={(checked) => setToolChecked(tool, checked === true)}
                    />
                    <Label htmlFor={toolId}>{tool}</Label>
                  </div>
                );
              })}
              {catalog.length === 0 ? (
                <p className="ext-empty">{tr("agents.tools.empty")}</p>
              ) : null}
            </div>
          ) : null}
          <Label className="ext-plugin-install__label" htmlFor="agent-max-turns">{tr("agents.maxTurns")}</Label>
          <Input id="agent-max-turns" className="settings-input" type="number" min={1} value={maxTurns} onChange={(event) => setMaxTurns(event.target.value)} />
          <AgentModelPicker locale={locale} value={createModel} providerGroups={providerGroups} onChange={setCreateModel} />
          <div className="ext-item__actions">
            <Button type="button" className="btn btn--ghost" disabled={busy} onClick={closeCreate}>{tr("common.cancel")}</Button>
            <Button type="button" className="btn btn--solid" disabled={!canCreate || busy} onClick={() => void createAgent()}>{busy ? tr("agents.creating") : tr("agents.create")}</Button>
          </div>
        </div>
      </GlassModal>

      <GlassModal open={!!detail} title={tr("agents.detailTitle")} onClose={() => setDetail(null)}>
        {detail ? <AgentDetailView locale={locale} detail={detail} /> : null}
      </GlassModal>

      <GlassModal open={!!removeTarget} title={tr("agents.removeTitle")} onClose={() => !busy && setRemoveTarget(null)}>
        <p>{tr("agents.removeConfirm", { name: removeTarget?.name ?? "" })}</p>
        <div className="ext-item__actions">
          <Button type="button" className="btn btn--ghost" disabled={busy} onClick={() => setRemoveTarget(null)}>{tr("common.cancel")}</Button>
          <Button type="button" className="btn btn--solid" disabled={busy} onClick={() => void removeAgent()}>{tr("agents.remove")}</Button>
        </div>
      </GlassModal>
    </div>
  );
}

import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Button } from "@/components/ui/button";
/** 设置 → 模型设置：管理自定义模型供应商及其模型列表。 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import { formatTokenCount } from "@/lib/contextUsage";
import { localizeUiError } from "@/lib/session";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Checkbox } from "@/components/ui/checkbox";
import { GlassModal } from "@/components/GlassModal";
import {
  IconEdit,
  IconPlus,
  IconRefresh,
  IconTrash,
} from "@/components/icons";

export interface ProvidersPanelProps {
  locale: Locale;
  /** 供应商配置变化后通知桌面外壳刷新模型列表。 */
  onProviderActivated?: () => void;
}

type FormState = {
  name: string;
  baseUrl: string;
  models: string[];
  modelDraft: string;
  /** 手动添加模型的上下文窗口输入（token）；空表示不配置。 */
  contextWindowDraft: string;
  maxOutputTokensDraft: string;
  /** 手动添加模型的 1M 开关。 */
  context1mDraft: boolean;
  /** 手动添加模型是否支持图片输入。 */
  supportsVisionDraft: boolean;
  apiKey: string;
  apiBackend: string;
  chatOutputTokenField: "max_completion_tokens" | "max_tokens";
  readTimeoutSeconds: string;
  /** 每模型手工配置的上下文窗口（token）；缺省表示自动获取或回退默认。 */
  contextWindows: Record<string, number>;
  maxOutputTokens: Record<string, number>;
  /** 启用 1M 上下文的模型集合；缺省表示不启用。 */
  context1m: Record<string, boolean>;
  /** 每模型是否支持图片输入。 */
  supportsVision: Record<string, boolean>;
};

type RightMode = "empty" | "create" | "edit";

type RemoteModel = {
  id: string;
  ownedBy?: string | null;
  /** 远端模型目录返回的上下文窗口；未提供时为空。 */
  contextWindow?: number | null;
  maxOutputTokens: number;
  supportsVision: boolean;
};

/** 创建空白供应商表单。 */
const emptyForm = (): FormState => ({
  name: "",
  baseUrl: "",
  models: [],
  modelDraft: "",
  contextWindowDraft: "",
  maxOutputTokensDraft: "128000",
  context1mDraft: false,
  supportsVisionDraft: false,
  apiKey: "",
  apiBackend: "responses",
  chatOutputTokenField: "max_completion_tokens",
  readTimeoutSeconds: "300",
  contextWindows: {},
  maxOutputTokens: {},
  context1m: {},
  supportsVision: {},
});

/** 提取供应商地址中的主机名。 */
function hostOf(url: string): string {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}

/** 模型设置主面板。 */
export function ProvidersPanel({
  locale,
  onProviderActivated,
}: ProvidersPanelProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [list, setList] = useState<api.ProvidersListResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selection, setSelection] = useState<string | null>(null);
  const [rightMode, setRightMode] = useState<RightMode>("empty");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState<FormState>(emptyForm);
  const [busy, setBusy] = useState(false);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [showKey, setShowKey] = useState(false);
  const [hint, setHint] = useState<string | null>(null);
  const [hintTone, setHintTone] = useState<"ok" | "err" | "muted">("muted");
  const [deleteTarget, setDeleteTarget] = useState<{
    id: string;
    name: string;
  } | null>(null);
  const [remoteModels, setRemoteModels] = useState<RemoteModel[]>([]);
  const [selectedRemoteModels, setSelectedRemoteModels] = useState<Set<string>>(
    new Set(),
  );
  const [modelPickerOpen, setModelPickerOpen] = useState(false);
  /** 手动添加弹窗只编辑当前供应商草稿，不直接保存配置。 */
  const [modelAddOpen, setModelAddOpen] = useState(false);
  const [modelEditTarget, setModelEditTarget] = useState<{ model: string; remote: boolean } | null>(null);
  /** 多选面板内的拉取错误；null 表示无错误。 */
  const [fetchError, setFetchError] = useState<string | null>(null);

  const protocolOptions = useMemo(
    () => [
      { value: "messages", label: tr("prov.protocol.messages") },
      {
        value: "chat_completions",
        label: tr("prov.protocol.chatCompletions"),
      },
      { value: "responses", label: tr("prov.protocol.responses") },
    ],
    [tr],
  );

  /** 将右侧详情切换到指定供应商。 */
  const openEdit = useCallback((provider: api.CustomProvider) => {
    setModelAddOpen(false);
    setSelection(provider.id);
    setEditingId(provider.id);
    setForm({
      name: provider.name,
      baseUrl: provider.baseUrl,
      models: [...provider.models],
      modelDraft: "",
      contextWindowDraft: "",
      maxOutputTokensDraft: "128000",
      context1mDraft: false,
      supportsVisionDraft: false,
      apiKey: provider.apiKey ?? "",
      apiBackend: provider.apiBackend,
      chatOutputTokenField: provider.chatOutputTokenField ?? "max_completion_tokens",
      readTimeoutSeconds: String(provider.readTimeoutSeconds ?? 300),
      contextWindows: { ...provider.contextWindows },
      maxOutputTokens: { ...provider.maxOutputTokens },
      context1m: { ...provider.context1m },
      supportsVision: { ...provider.supportsVision },
    });
    setHint(null);
    setShowKey(false);
    setRightMode("edit");
  }, []);

  /** 读取供应商列表，并默认选中第一项。 */
  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      if (!api.isTauri()) {
        setList({
          providers: [],
          defaultModel: null,
          activeProviderId: null,
        });
        return;
      }
      const result = await api.providersList();
      setList(result);
      if (result.providers[0]) {
        openEdit(result.providers[0]);
      }
    } catch (loadError) {
      setError(localizeUiError(loadError, locale));
    } finally {
      setLoading(false);
    }
  }, [openEdit]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const providers = list?.providers ?? [];

  /** 打开新增供应商表单。 */
  const openCreate = () => {
    setModelAddOpen(false);
    setSelection(null);
    setEditingId(null);
    setForm(emptyForm());
    setHint(null);
    setShowKey(false);
    setRightMode("create");
  };

  /** 仅切换当前表单中 API Key 的可见性。 */
  const toggleKeyVisibility = () => setShowKey((current) => !current);

  const draftEdited = useRef(new Set<string>());
  const [loadingMetadata, setLoadingMetadata] = useState(false);
  useEffect(() => {
    if (!modelAddOpen || modelEditTarget || !form.modelDraft.trim()) {
      setLoadingMetadata(false);
      return;
    }
    let cancelled = false;
    setLoadingMetadata(true);
    const timer = setTimeout(async () => {
      try {
        const [item] = await api.modelMetadataGetMany([form.modelDraft.trim()]);
        if (!cancelled) setForm((current) => ({
          ...current,
          ...(!draftEdited.current.has("context") ? { contextWindowDraft: String(item?.contextWindow ?? ""), context1mDraft: false } : {}),
          ...(!draftEdited.current.has("output") ? { maxOutputTokensDraft: String(item?.maxOutputTokens ?? 128000) } : {}),
          ...(!draftEdited.current.has("vision") ? { supportsVisionDraft: item?.supportsVision ?? false } : {}),
        }));
      } catch {
        // 目录不可用时保留当前输入及默认输出预算。
      } finally {
        if (!cancelled) setLoadingMetadata(false);
      }
    }, 400);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [modelAddOpen, modelEditTarget, form.modelDraft]);

  /** 每次打开使用空白草稿，关闭后不会把未提交输入带入下一次添加。 */
  const openAddModel = () => {
    setModelEditTarget(null);
    draftEdited.current.clear();
    setForm((current) => ({
      ...current,
      modelDraft: "",
      contextWindowDraft: "",
      maxOutputTokensDraft: "128000",
      context1mDraft: false,
      supportsVisionDraft: false,
    }));
    setModelAddOpen(true);
  };

  const openModelEditor = (model: string, remote = false) => {
    const item = remote ? remoteModels.find((entry) => entry.id === model) : undefined;
    setModelEditTarget({ model, remote });
    setLoadingMetadata(false);
    setForm((current) => ({
      ...current,
      modelDraft: model,
      contextWindowDraft: String(remote ? item?.contextWindow ?? "" : current.contextWindows[model] ?? ""),
      maxOutputTokensDraft: String(remote ? item?.maxOutputTokens ?? 128000 : current.maxOutputTokens[model] ?? 128000),
      context1mDraft: remote ? false : Boolean(current.context1m[model]),
      supportsVisionDraft: remote ? Boolean(item?.supportsVision) : Boolean(current.supportsVision[model]),
    }));
    if (remote) setModelPickerOpen(false);
    setModelAddOpen(true);
  };

  const closeModelEditor = () => {
    setModelAddOpen(false);
    if (modelEditTarget?.remote) setModelPickerOpen(true);
    setModelEditTarget(null);
  };

  /** 将弹窗草稿加入模型列表；供应商保存流程保持不变。 */
  const addDraftModel = () => {
    const model = form.modelDraft.trim();
    if (!model || busy || loadingMetadata) return;
    if (modelEditTarget?.remote) {
      setRemoteModels((current) => current.map((item) => item.id === model ? {
        ...item,
        contextWindow: form.context1mDraft ? 1000000 : Number(form.contextWindowDraft) || null,
        maxOutputTokens: Number(form.maxOutputTokensDraft),
        supportsVision: form.supportsVisionDraft,
      } : item));
      closeModelEditor();
      return;
    }
    setForm((current) => {
      const contextWindows = { ...current.contextWindows };
      const draft = Number.parseInt(current.contextWindowDraft, 10);
      if (Number.isFinite(draft) && draft > 0) {
        contextWindows[model] = draft;
      } else {
        delete contextWindows[model];
      }
      const context1m = { ...current.context1m };
      if (current.context1mDraft) {
        context1m[model] = true;
      } else {
        delete context1m[model];
      }
      const supportsVision = {
        ...current.supportsVision,
        [model]: current.supportsVisionDraft,
      };
      return {
        ...current,
        models: current.models.includes(model)
          ? current.models
          : [...current.models, model],
        modelDraft: "",
        contextWindowDraft: "",
      maxOutputTokensDraft: "128000",
        context1mDraft: false,
        supportsVisionDraft: false,
        contextWindows,
        maxOutputTokens: { ...current.maxOutputTokens, [model]: Number(current.maxOutputTokensDraft) },
        context1m,
        supportsVision,
      };
    });
    closeModelEditor();

  };

  /** 从模型列表移除一个模型。 */
  const removeModel = (model: string) => {
    setForm((current) => {
      const contextWindows = { ...current.contextWindows };
      delete contextWindows[model];
      const maxOutputTokens = { ...current.maxOutputTokens };
      delete maxOutputTokens[model];
      const context1m = { ...current.context1m };
      delete context1m[model];
      const supportsVision = { ...current.supportsVision };
      delete supportsVision[model];
      return {
        ...current,
        models: current.models.filter((item) => item !== model),
        contextWindows,
        maxOutputTokens,
        context1m,
        supportsVision,
      };
    });
  };

  /** 保存新增或编辑后的供应商。 */
  const save = async () => {
    if (!form.name.trim()) {
      setHint(tr("prov.err.needDescription"));
      setHintTone("err");
      return;
    }
    if (!form.baseUrl.trim()) {
      setHint(tr("prov.err.needBase"));
      setHintTone("err");
      return;
    }
    if (form.models.length === 0) {
      setHint(tr("prov.err.needModel"));
      setHintTone("err");
      return;
    }
    if (form.models.some((model) => !Number.isInteger(form.maxOutputTokens[model] ?? 128000) || (form.maxOutputTokens[model] ?? 128000) < 1 || (form.maxOutputTokens[model] ?? 128000) > 4294967295)) {
      setHint(tr("prov.err.outputTokens"));
      setHintTone("err");
      return;
    }
    const readTimeoutSeconds = Number(form.readTimeoutSeconds);
    if (!Number.isInteger(readTimeoutSeconds) || readTimeoutSeconds < 1 || readTimeoutSeconds > 3600) {
      setHint(tr("prov.err.readTimeout"));
      setHintTone("err");
      return;
    }
    setBusy(true);
    setHint(tr("prov.saving"));
    setHintTone("muted");
    try {
      const id = editingId ?? globalThis.crypto.randomUUID();
      const result = await api.providersUpsert({
        id,
        models: form.models,
        baseUrl: form.baseUrl.trim(),
        name: form.name.trim(),
        apiKey: form.apiKey === "" ? undefined : form.apiKey,
        apiBackend: form.apiBackend,
        chatOutputTokenField: form.chatOutputTokenField,
        readTimeoutSeconds,
        contextWindows: form.contextWindows,
        maxOutputTokens: Object.fromEntries(form.models.map((model) => [model, form.maxOutputTokens[model] ?? 128000])),
        context1m: form.context1m,
        supportsVision: form.supportsVision,
        createOnly: !editingId,
      });
      setList(result);
      const saved = result.providers.find((provider) => provider.id === id);
      if (saved) {
        openEdit(saved);
      }
      onProviderActivated?.();
    } catch (saveError) {
      setHint(String(saveError));
      setHintTone("err");
    } finally {
      setBusy(false);
    }
  };

  /** 删除当前供应商并选中剩余列表第一项。 */
  const confirmRemove = async () => {
    if (!deleteTarget) return;
    setBusy(true);
    setDeleteTarget(null);
    try {
      const result = await api.providersRemove(deleteTarget.id);
      setList(result);
      if (result.providers[0]) {
        openEdit(result.providers[0]);
      } else {
        setSelection(null);
        setEditingId(null);
        setRightMode("empty");
        setForm(emptyForm());
        setShowKey(false);
      }
      onProviderActivated?.();
    } catch (removeError) {
      setError(localizeUiError(removeError, locale));
    } finally {
      setBusy(false);
    }
  };

  /** 拉取远端模型：立即打开多选面板，加载/空/错误状态都在面板内展示。 */
  const fetchModels = async () => {
    if (!form.baseUrl.trim()) {
      setHint(tr("prov.err.needBase"));
      setHintTone("err");
      return;
    }
    setBusy(true);
    setFetchingModels(true);
    setFetchError(null);
    setModelPickerOpen(true);
    try {
      const result = await api.providersListModels({
        baseUrl: form.baseUrl.trim(),
        apiKey: form.apiKey === "" ? undefined : form.apiKey,
        providerId: editingId ?? undefined,
        apiBackend: form.apiBackend,
      });
      const metadata = new Map<string, api.ModelMetadata>();
      // 元数据接口每批最多接受 256 个模型，避免大型供应商目录整批降级为默认值。
      for (let offset = 0; offset < result.models.length; offset += 256) {
        const ids = result.models.slice(offset, offset + 256).map((model) => model.id);
        const items = await api.modelMetadataGetMany(ids).catch(() => []);
        for (const item of items) metadata.set(item.modelId, item);
      }
      const models = result.models.map((model) => ({
        id: model.id,
        ownedBy: model.ownedBy,
        contextWindow: form.context1m[model.id] ? 1000000 : form.contextWindows[model.id] ?? metadata.get(model.id)?.contextWindow ?? model.contextWindow,
        maxOutputTokens: form.maxOutputTokens[model.id] ?? metadata.get(model.id)?.maxOutputTokens ?? 128000,
        supportsVision: form.supportsVision[model.id] ?? metadata.get(model.id)?.supportsVision ?? false,
      }));
      setRemoteModels(models);
      setSelectedRemoteModels(
        new Set(
          models
            .map((model) => model.id)
            .filter((model) => form.models.includes(model)),
        ),
      );
    } catch (fetchError) {
      setFetchError(localizeUiError(fetchError, locale));
    } finally {
      setBusy(false);
      setFetchingModels(false);
    }
  };

  /** 切换远端模型的勾选状态。 */
  const toggleRemoteModel = (model: string) => {
    setSelectedRemoteModels((current) => {
      const next = new Set(current);
      if (next.has(model)) next.delete(model);
      else next.add(model);
      return next;
    });
  };

  /** 全选或取消全选远端返回的模型。 */
  const toggleAllRemoteModels = () => {
    setSelectedRemoteModels((current) =>
      current.size === remoteModels.length
        ? new Set()
        : new Set(remoteModels.map((model) => model.id)),
    );
  };

  /** 将用户勾选的远端模型合并到供应商模型列表。 */
  const applyRemoteModels = () => {
    const selected = [...selectedRemoteModels];
    setForm((current) => {
      const contextWindows = { ...current.contextWindows };
      const context1m = { ...current.context1m };
      const supportsVision = { ...current.supportsVision };
      const maxOutputTokens = { ...current.maxOutputTokens };
      for (const model of remoteModels.filter((item) => selectedRemoteModels.has(item.id))) {
        if (model.contextWindow) contextWindows[model.id] = model.contextWindow;
        else delete contextWindows[model.id];
        delete context1m[model.id];
        supportsVision[model.id] = model.supportsVision;
        maxOutputTokens[model.id] = model.maxOutputTokens;
      }
      return {
        ...current,
        models: [
          ...current.models,
          ...selected.filter((model) => !current.models.includes(model)),
        ],
        contextWindows,
        maxOutputTokens,
        context1m,
        supportsVision,
      };
    });

    setModelPickerOpen(false);
  };

  if (loading) {
    return (
      <div className="prov-panel" data-testid="providers-panel">
        <div className="prov-loading">{tr("prov.loading")}</div>
      </div>
    );
  }

  return (
    <div className="prov-panel" data-testid="providers-panel">
      {error && (
        <div className="prov-alert" role="alert">
          <span>{error}</span>
          <Button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={() => setError(null)}
          >
            {tr("common.dismiss")}
          </Button>
        </div>
      )}

      <div className="prov-split">
        <aside className="prov-split__list">
          <Button
            type="button"
            className="btn btn--solid prov-add-btn"
            onClick={openCreate}
            disabled={busy}
          >
            <IconPlus size={16} />
            {tr("prov.new")}
          </Button>

          <div className="prov-rail" role="list">
            {providers.map((provider) => (
              <div
                key={provider.id}
                role="listitem"
                className={
                  "prov-item" +
                  (selection === provider.id ? " is-selected" : "")
                }
              >
                <Button
                  type="button"
                  className="prov-item__main"
                  onClick={() => openEdit(provider)}
                >
                  <span className="prov-item__avatar" aria-hidden>
                    {(provider.name || provider.id).slice(0, 1).toUpperCase()}
                  </span>
                  <span className="prov-item__text">
                    <span className="prov-item__name">
                      {provider.name || provider.id}
                    </span>
                    <span className="prov-item__sub">
                      {hostOf(provider.baseUrl)} ·{" "}
                      {tr("prov.modelCount", { n: provider.models.length })}
                    </span>
                  </span>
                </Button>
              </div>
            ))}

            {providers.length === 0 && (
              <div className="prov-rail-empty">{tr("prov.emptyTitle")}</div>
            )}
          </div>
        </aside>

        <section className="prov-split__detail">
          {rightMode === "empty" && (
            <div className="prov-detail-empty">
              <p>{tr("prov.detailEmpty")}</p>
            </div>
          )}

          {(rightMode === "create" || rightMode === "edit") && (
            <div
              className="prov-detail settings-card prov-form"
              data-testid="provider-form"
            >
              <div className="prov-form__head">
                <h3 className="prov-detail__title">
                  {editingId ? tr("prov.editTitle") : tr("prov.addTitle")}
                </h3>
              </div>

              <div className="prov-form__grid">
                <Label className="prov-field">
                  <span className="prov-field__label">{tr("prov.name")}</span>
                  <Input
                    className="settings-input"
                    value={form.name}
                    onChange={(event) =>
                      setForm((current) => ({
                        ...current,
                        name: event.target.value,
                      }))
                    }
                    placeholder={tr("prov.namePh")}
                    autoComplete="off"
                  />
                </Label>

                <Label className="prov-field prov-field--full">
                  <span className="prov-field__label">{tr("prov.baseUrl")}</span>
                  <Input
                    className="settings-input"
                    value={form.baseUrl}
                    onChange={(event) =>
                      setForm((current) => ({
                        ...current,
                        baseUrl: event.target.value,
                      }))
                    }
                    placeholder={tr("prov.baseUrlPh")}
                    autoComplete="off"
                    spellCheck={false}
                  />
                </Label>

                <div className="prov-field">
                  <span className="prov-field__label">{tr("prov.protocol")}</span>
                  <Select
                    value={form.apiBackend}
                    onValueChange={(value) =>
                      setForm((current) => ({
                        ...current,
                        apiBackend: value,
                      }))
                    }
                  >
                    <SelectTrigger
                      className="settings-input"
                      aria-label={tr("prov.protocol")}
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectGroup>
                        {protocolOptions.map((option) => (
                          <SelectItem key={option.value} value={option.value}>
                            {option.label}
                          </SelectItem>
                        ))}
                      </SelectGroup>
                    </SelectContent>
                  </Select>
                </div>

                {form.apiBackend === "chat_completions" && (
                  <div className="prov-field">
                    <span className="prov-field__label">{tr("prov.chatOutputTokenField")}</span>
                    <Select value={form.chatOutputTokenField} onValueChange={(value) => setForm((current) => ({ ...current, chatOutputTokenField: value as FormState["chatOutputTokenField"] }))}>
                      <SelectTrigger className="settings-input" aria-label={tr("prov.chatOutputTokenField")}><SelectValue /></SelectTrigger>
                      <SelectContent><SelectGroup>
                        <SelectItem value="max_completion_tokens">max_completion_tokens</SelectItem>
                        <SelectItem value="max_tokens">max_tokens</SelectItem>
                      </SelectGroup></SelectContent>
                    </Select>
                    <span className="prov-field__hint">{tr("prov.chatOutputTokenFieldHint")}</span>
                  </div>
                )}
                <Label className="prov-field">
                  <span className="prov-field__label">{tr("prov.readTimeout")}</span>
                  <Input className="settings-input" type="number" min={1} max={3600} value={form.readTimeoutSeconds} onChange={(event) => setForm((current) => ({ ...current, readTimeoutSeconds: event.target.value }))} />
                  <span className="prov-field__hint">{tr("prov.readTimeoutHint")}</span>
                </Label>

                <Label className="prov-field">
                  <span className="prov-field__label">{tr("prov.apiKey")}</span>
                  <div className="prov-key-row">
                    <Input
                      className="settings-input"
                      type={showKey ? "text" : "password"}
                      value={form.apiKey}
                      onChange={(event) => {
                        setForm((current) => ({
                          ...current,
                          apiKey: event.target.value,
                        }));
                      }}
                      placeholder={tr("prov.keyPh")}
                      autoComplete="new-password"
                      spellCheck={false}
                    />
                    <Button
                      type="button"
                      className="btn btn--ghost btn--sm"
                      onClick={toggleKeyVisibility}
                    >
                      {showKey ? tr("prov.keyHide") : tr("prov.keyShow")}
                    </Button>
                  </div>
                  <span className="prov-field__hint">
                    {tr("prov.keyStorageHint")}
                  </span>
                </Label>

                <div className="prov-field prov-field--full">
                  <span className="prov-field__label-row prov-model-toolbar">
                    <span className="prov-field__label">
                      {tr("prov.modelList")}
                    </span>
                    <span className="prov-model-actions">
                      <Button
                        type="button"
                        className={
                          "btn btn--ghost btn--sm prov-fetch-button" +
                          (fetchingModels ? " is-loading" : "")
                        }
                        onClick={() => void fetchModels()}
                        disabled={busy}
                      >
                        <IconRefresh size={14} />
                        {fetchingModels
                          ? tr("prov.fetching")
                          : tr("prov.fetchModels")}
                      </Button>
                      <Button
                        type="button"
                        className="btn btn--ghost btn--sm"
                        onClick={openAddModel}
                        disabled={busy}
                      >
                        <IconPlus size={14} />
                        {tr("prov.addModel")}
                      </Button>
                    </span>
                  </span>
                  <div className="prov-model-list" role="list">
                    {form.models.map((model) => (
                      <div className="prov-model-row" role="listitem" key={model}>
                        <span className="prov-model-row__name" title={model}>
                          {model}
                        </span>
                        {form.supportsVision[model] && <span>{tr("prov.supportsVision")}</span>}
                        {(form.context1m[model] || form.contextWindows[model]) && (
                          <span title={tr("prov.contextWindowFor", { model })}>
                            {formatTokenCount(form.context1m[model] ? 1000000 : form.contextWindows[model]!)}
                          </span>
                        )}
                        <Button type="button" variant="icon"
                          aria-label={`${tr("prov.editModel")} ${model}`}
                          onClick={() => openModelEditor(model)}>
                          <IconEdit size={14} />
                        </Button>
                        <Button
                          type="button"
                          className="tree-icon-btn"
                          onClick={() => removeModel(model)}
                          aria-label={tr("prov.removeModel", { model })}
                        >
                          <IconTrash size={13} />
                        </Button>
                      </div>
                    ))}
                    {form.models.length === 0 && (
                      <div className="prov-model-empty">
                        {tr("prov.modelListEmpty")}
                      </div>
                    )}
                  </div>
                </div>
              </div>

              {hint && (
                <div
                  className={
                    "prov-form__hint" +
                    (hintTone === "ok"
                      ? " is-ok"
                      : hintTone === "err"
                        ? " is-err"
                        : "")
                  }
                >
                  {hint}
                </div>
              )}

              <div className="prov-form__actions">
                {editingId && (
                  <Button
                    type="button"
                    className="btn btn--danger"
                    disabled={busy}
                    onClick={() =>
                      setDeleteTarget({
                        id: editingId,
                        name: form.name || editingId,
                      })
                    }
                  >
                    <IconTrash size={14} />
                    {tr("prov.delete")}
                  </Button>
                )}
                <div className="prov-form__actions-end">
                  {rightMode === "create" && providers[0] ? (
                    <Button
                      type="button"
                      className="btn btn--ghost"
                      onClick={() => openEdit(providers[0]!)}
                      disabled={busy}
                    >
                      {tr("common.cancel")}
                    </Button>
                  ) : null}
                  <Button
                    type="button"
                    className="btn btn--solid"
                    onClick={() => void save()}
                    disabled={busy}
                  >
                    {editingId ? (
                      <>
                        <IconEdit size={14} />
                        {tr("prov.save")}
                      </>
                    ) : (
                      <>
                        <IconPlus size={14} />
                        {tr("prov.add")}
                      </>
                    )}
                  </Button>
                </div>
              </div>
            </div>
          )}
        </section>
      </div>

      <GlassModal
        open={modelAddOpen}
        onClose={closeModelEditor}
        title={modelEditTarget ? tr("prov.editModel") : tr("prov.addModel")}
        size="md"
        closeLabel={tr("common.close")}
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              onClick={closeModelEditor}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              type="submit"
              className="btn btn--solid"
              form="provider-add-model-form"
              disabled={busy || loadingMetadata || !form.modelDraft.trim()}
            >
              {modelEditTarget ? tr("prov.applyModel") : tr("prov.addModel")}
            </Button>
          </>
        }
      >
        <form
          id="provider-add-model-form"
          className="prov-model-add-form"
          onSubmit={(event) => {
            event.preventDefault();
            addDraftModel();
          }}
        >
          <div className="prov-field">
            <Label htmlFor="provider-model-name-draft">{tr("prov.modelId")}</Label>
            <Input
              id="provider-model-name-draft"
              className="settings-input"
              data-modal-autofocus
              required
              readOnly={Boolean(modelEditTarget)}
              value={form.modelDraft}
              onChange={(event) => {
                draftEdited.current.clear();
                setForm((current) => ({
                  ...current,
                  modelDraft: event.target.value,
                  contextWindowDraft: "",
                  context1mDraft: false,
                  supportsVisionDraft: false,
                  maxOutputTokensDraft: "128000",
                }));
              }}
              placeholder={tr("prov.modelPh")}
              autoComplete="off"
              spellCheck={false}
            />
          </div>
          <div className="prov-field">
            <Label htmlFor="provider-model-context-draft">{tr("prov.contextWindow")}</Label>
            <Input
              id="provider-model-context-draft"
              className="settings-input"
              type="number"
              min={1024}
              max={10000000}
              step="any"
              inputMode="numeric"
              value={form.contextWindowDraft}
              onChange={(event) => { draftEdited.current.add("context"); setForm((current) => ({ ...current, contextWindowDraft: event.target.value })); }}
              placeholder={tr("prov.contextWindowPh")}
            />
          </div>
          <div className="prov-field">
            <Label htmlFor="provider-model-output-draft">{tr("prov.maxOutputTokens")}</Label>
            <Input variant="settings" id="provider-model-output-draft" type="number" min={1} max={4294967295} step={1} required
              value={form.maxOutputTokensDraft}
              onChange={(event) => { draftEdited.current.add("output"); setForm((current) => ({ ...current, maxOutputTokensDraft: event.target.value })); }} />
          </div>
          <div className="prov-model-options">
            <div className="prov-model-option">
              <Checkbox
                id="provider-model-context-1m-draft"
                className="size-[14px] cursor-pointer"
                checked={form.context1mDraft}
                onCheckedChange={(checked) => {
                  draftEdited.current.add("context");
                  setForm((current) => ({
                    ...current,
                    context1mDraft: checked === true,
                    contextWindowDraft:
                      checked === true ? "" : current.contextWindowDraft,
                  }));
                }}
              />
              <Label htmlFor="provider-model-context-1m-draft">1M</Label>
            </div>
            <div className="prov-model-option">
              <Checkbox
                id="provider-model-vision-draft"
                className="size-[14px] cursor-pointer"
                checked={form.supportsVisionDraft}
                onCheckedChange={(checked) => {
                  draftEdited.current.add("vision");
                  setForm((current) => ({ ...current, supportsVisionDraft: checked === true }));
                }}
              />
              <Label htmlFor="provider-model-vision-draft">{tr("prov.supportsVision")}</Label>
            </div>
          </div>
          <p className="prov-field__hint">{modelEditTarget ? tr("prov.modelEditHint") : tr("prov.modelAddHint")}</p>
        </form>
      </GlassModal>

      <GlassModal
        open={modelPickerOpen}
        onClose={() => setModelPickerOpen(false)}
        title={tr("prov.modelPickerTitle")}
        size="md"
        closeLabel={tr("common.close")}
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              onClick={() => setModelPickerOpen(false)}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              type="button"
              className="btn btn--solid"
              onClick={applyRemoteModels}
              disabled={fetchingModels || selectedRemoteModels.size === 0 || remoteModels.some((model) => selectedRemoteModels.has(model.id) && (!Number.isInteger(model.maxOutputTokens) || model.maxOutputTokens < 1 || model.maxOutputTokens > 4294967295 || (model.contextWindow != null && (!Number.isInteger(model.contextWindow) || model.contextWindow < 1024 || model.contextWindow > 10000000))))}
            >
              {tr("prov.addSelected", { n: selectedRemoteModels.size })}
            </Button>
          </>
        }
      >
        {fetchingModels ? (
          <div className="prov-model-empty">{tr("prov.fetching")}</div>
        ) : fetchError ? (
          <div className="prov-model-empty is-err">{fetchError}</div>
        ) : remoteModels.length > 0 ? (
          <>
            <div className="prov-model-picker__select-all">
              <Checkbox
                id="provider-remote-model-select-all"
                className="size-[14px] cursor-pointer"
                checked={selectedRemoteModels.size === remoteModels.length}
                aria-label={
                  selectedRemoteModels.size === remoteModels.length
                    ? tr("prov.deselectAll")
                    : tr("prov.selectAll")
                }
                onCheckedChange={toggleAllRemoteModels}
              />
              <Label htmlFor="provider-remote-model-select-all">
                {selectedRemoteModels.size === remoteModels.length
                  ? tr("prov.deselectAll")
                  : tr("prov.selectAll")}
              </Label>
              <span className="prov-model-picker__count">
                {selectedRemoteModels.size}/{remoteModels.length}
              </span>
            </div>
            <div className="prov-model-picker" role="list">
              {remoteModels.map((model) => (
                <div className="prov-field" role="listitem" key={model.id}>
                <div
                  className="prov-model-picker__row"
                >
                  <Checkbox
                    id={`provider-remote-model-${encodeURIComponent(model.id)}`}
                    className="size-[14px] cursor-pointer"
                    checked={selectedRemoteModels.has(model.id)}
                    aria-label={model.id}
                    onCheckedChange={() => toggleRemoteModel(model.id)}
                  />
                  <Label
                    className="prov-model-picker__name"
                    htmlFor={`provider-remote-model-${encodeURIComponent(model.id)}`}
                  >
                    {model.id}
                  </Label>
                  <div className="flex items-center justify-end gap-2">
                    {model.supportsVision && <span>{tr("prov.supportsVision")}</span>}
                    {model.contextWindow != null && <span title={tr("prov.contextWindowFor", { model: model.id })}>{formatTokenCount(model.contextWindow)}</span>}
                    <Button type="button" variant="icon"
                      aria-label={`${tr("prov.editModel")} ${model.id}`}
                      onClick={() => openModelEditor(model.id, true)}>
                      <IconEdit size={14} />
                    </Button>
                  </div>
                </div>
                </div>
              ))}
            </div>
          </>
        ) : (
          <div className="prov-model-empty">{tr("prov.emptyList")}</div>
        )}
      </GlassModal>

      <GlassModal
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        title={tr("prov.delete")}
        size="sm"
        closeLabel={tr("common.close")}
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              onClick={() => setDeleteTarget(null)}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              type="button"
              className="btn btn--danger"
              onClick={() => void confirmRemove()}
            >
              {tr("prov.delete")}
            </Button>
          </>
        }
      >
        <p className="prov-delete-msg">
          {tr("prov.confirmDelete", {
            id: deleteTarget?.name || deleteTarget?.id || "",
          })}
        </p>
      </GlassModal>
    </div>
  );
}

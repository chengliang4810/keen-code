/** 设置 → 模型设置：管理自定义模型供应商及其模型列表。 */

import { useCallback, useEffect, useMemo, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
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
  /** 手动添加模型的 1M 开关。 */
  context1mDraft: boolean;
  apiKey: string;
  apiBackend: string;
  /** 每模型手工配置的上下文窗口（token）；缺省表示自动获取或回退默认。 */
  contextWindows: Record<string, number>;
  /** 启用 1M 上下文的模型集合；缺省表示不启用。 */
  context1m: Record<string, boolean>;
};

type RightMode = "empty" | "create" | "edit";

type RemoteModel = {
  id: string;
  ownedBy?: string | null;
  /** 远端模型目录返回的上下文窗口；未提供时为空。 */
  contextWindow?: number | null;
};

/** 创建空白供应商表单。 */
const emptyForm = (): FormState => ({
  name: "",
  baseUrl: "",
  models: [],
  modelDraft: "",
  contextWindowDraft: "",
  context1mDraft: false,
  apiKey: "",
  apiBackend: "responses",
  contextWindows: {},
  context1m: {},
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
  /** 多选面板内的拉取错误；null 表示无错误。 */
  const [fetchError, setFetchError] = useState<string | null>(null);

  const protocolOptions = useMemo(
    () => [
      { value: "responses", label: tr("prov.protocol.responses") },
      {
        value: "chat_completions",
        label: tr("prov.protocol.chatCompletions"),
      },
      { value: "messages", label: tr("prov.protocol.messages") },
    ],
    [tr],
  );

  /** 将右侧详情切换到指定供应商。 */
  const openEdit = useCallback((provider: api.CustomProvider) => {
    setSelection(provider.id);
    setEditingId(provider.id);
    setForm({
      name: provider.name,
      baseUrl: provider.baseUrl,
      models: [...provider.models],
      modelDraft: "",
      contextWindowDraft: "",
      context1mDraft: false,
      apiKey: provider.apiKey ?? "",
      apiBackend: provider.apiBackend,
      contextWindows: { ...provider.contextWindows },
      context1m: { ...provider.context1m },
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
      setError(String(loadError));
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
    setSelection(null);
    setEditingId(null);
    setForm(emptyForm());
    setHint(null);
    setShowKey(false);
    setRightMode("create");
  };

  /** 仅切换当前表单中 API Key 的可见性。 */
  const toggleKeyVisibility = () => setShowKey((current) => !current);

  /** 将输入框中的模型（含上下文窗口与 1M 开关）加入模型列表。 */
  const addDraftModel = () => {
    const model = form.modelDraft.trim();
    if (!model) return;
    setForm((current) => {
      const contextWindows = { ...current.contextWindows };
      const draft = Number.parseInt(current.contextWindowDraft, 10);
      if (Number.isFinite(draft) && draft > 0) {
        contextWindows[model] = draft;
      }
      const context1m = { ...current.context1m };
      if (current.context1mDraft) {
        context1m[model] = true;
      }
      return {
        ...current,
        models: current.models.includes(model)
          ? current.models
          : [...current.models, model],
        modelDraft: "",
        contextWindowDraft: "",
        context1mDraft: false,
        contextWindows,
        context1m,
      };
    });
  };

  /** 从模型列表移除一个模型。 */
  const removeModel = (model: string) => {
    setForm((current) => {
      const contextWindows = { ...current.contextWindows };
      delete contextWindows[model];
      const context1m = { ...current.context1m };
      delete context1m[model];
      return {
        ...current,
        models: current.models.filter((item) => item !== model),
        contextWindows,
        context1m,
      };
    });
  };

  /** 切换单个模型的 1M 上下文开关。 */
  const toggleModelContext1m = (model: string) => {
    setForm((current) => ({
      ...current,
      context1m: {
        ...current.context1m,
        [model]: !current.context1m[model],
      },
    }));
  };

  /** 更新单个模型的上下文窗口配置；空值表示不配置（自动获取或回退默认）。 */
  const setModelContextWindow = (model: string, raw: string) => {
    setForm((current) => {
      const value = Number.parseInt(raw, 10);
      if (!raw.trim() || !Number.isFinite(value) || value <= 0) {
        const { [model]: _removed, ...rest } = current.contextWindows;
        return { ...current, contextWindows: rest };
      }
      return {
        ...current,
        contextWindows: { ...current.contextWindows, [model]: value },
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
        contextWindows: form.contextWindows,
        context1m: form.context1m,
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
      setError(String(removeError));
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
      const models = result.models.map((model) => ({
        id: model.id,
        ownedBy: model.ownedBy,
        contextWindow: model.contextWindow,
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
      setFetchError(String(fetchError));
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
    // 勾选时若远端返回了上下文窗口且未手工配置，自动预填（用户可随后修改）。
    const remoteContextWindow = remoteModels.find(
      (item) => item.id === model,
    )?.contextWindow;
    if (remoteContextWindow) {
      setForm((current) => {
        if (current.contextWindows[model] !== undefined) return current;
        return {
          ...current,
          contextWindows: {
            ...current.contextWindows,
            [model]: remoteContextWindow,
          },
        };
      });
    }
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
    setForm((current) => ({
      ...current,
      models: [
        ...current.models,
        ...[...selectedRemoteModels].filter(
          (model) => !current.models.includes(model),
        ),
      ],
    }));
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
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={() => setError(null)}
          >
            {tr("common.dismiss")}
          </button>
        </div>
      )}

      <div className="prov-split">
        <aside className="prov-split__list">
          <button
            type="button"
            className="btn btn--solid prov-add-btn"
            onClick={openCreate}
            disabled={busy}
          >
            <IconPlus size={16} />
            {tr("prov.new")}
          </button>

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
                <button
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
                </button>
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
                <label className="prov-field">
                  <span className="prov-field__label">{tr("prov.name")}</span>
                  <input
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
                </label>

                <label className="prov-field prov-field--full">
                  <span className="prov-field__label">{tr("prov.baseUrl")}</span>
                  <input
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
                </label>

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

                <label className="prov-field">
                  <span className="prov-field__label">{tr("prov.apiKey")}</span>
                  <div className="prov-key-row">
                    <input
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
                    <button
                      type="button"
                      className="btn btn--ghost btn--sm"
                      onClick={toggleKeyVisibility}
                    >
                      {showKey ? tr("prov.keyHide") : tr("prov.keyShow")}
                    </button>
                  </div>
                  <span className="prov-field__hint">
                    {tr("prov.keyStorageHint")}
                  </span>
                </label>

                <div className="prov-field prov-field--full">
                  <span className="prov-field__label-row">
                    <span className="prov-field__label">
                      {tr("prov.modelList")}
                    </span>
                    <button
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
                    </button>
                  </span>
                  <div className="prov-model-add">
                    <input
                      className="settings-input"
                      value={form.modelDraft}
                      onChange={(event) =>
                        setForm((current) => ({
                          ...current,
                          modelDraft: event.target.value,
                        }))
                      }
                      onKeyDown={(event) => {
                        if (event.key !== "Enter") return;
                        event.preventDefault();
                        addDraftModel();
                      }}
                      placeholder={tr("prov.modelPh")}
                      autoComplete="off"
                      spellCheck={false}
                    />
                    <input
                      className="prov-model-add__context"
                      type="number"
                      min={1024}
                      max={10000000}
                      step={1000}
                      inputMode="numeric"
                      value={form.contextWindowDraft}
                      onChange={(event) =>
                        setForm((current) => ({
                          ...current,
                          contextWindowDraft: event.target.value,
                        }))
                      }
                      aria-label={tr("prov.contextWindow")}
                      placeholder={tr("prov.contextWindowPh")}
                    />
                    <div className="prov-model-add__1m">
                      <Checkbox
                        id="provider-model-context-1m-draft"
                        className="size-[14px] cursor-pointer"
                        checked={form.context1mDraft}
                        aria-label="1M"
                        onCheckedChange={(checked) =>
                          setForm((current) => ({
                            ...current,
                            context1mDraft: checked === true,
                          }))
                        }
                      />
                      <label htmlFor="provider-model-context-1m-draft">1M</label>
                    </div>
                    <button
                      type="button"
                      className="btn btn--ghost"
                      onClick={addDraftModel}
                    >
                      <IconPlus size={14} />
                      {tr("prov.addModel")}
                    </button>
                  </div>
                  <div className="prov-model-list" role="list">
                    {form.models.map((model) => (
                      <div className="prov-model-row" role="listitem" key={model}>
                        <span className="prov-model-row__name" title={model}>
                          {model}
                        </span>
                        <input
                          className="prov-model-row__context"
                          type="number"
                          min={1024}
                          max={10000000}
                          step={1000}
                          inputMode="numeric"
                          value={form.contextWindows[model] ?? ""}
                          onChange={(event) =>
                            setModelContextWindow(model, event.target.value)
                          }
                          aria-label={tr("prov.contextWindowFor", { model })}
                          placeholder={tr("prov.contextWindowPh")}
                        />
                        <div className="prov-model-row__1m">
                          <Checkbox
                            id={`provider-model-context-1m-${encodeURIComponent(model)}`}
                            className="size-[14px] cursor-pointer"
                            checked={Boolean(form.context1m[model])}
                            aria-label="1M"
                            onCheckedChange={() => toggleModelContext1m(model)}
                          />
                          <label htmlFor={`provider-model-context-1m-${encodeURIComponent(model)}`}>
                            1M
                          </label>
                        </div>
                        <button
                          type="button"
                          className="tree-icon-btn"
                          onClick={() => removeModel(model)}
                          aria-label={tr("prov.removeModel", { model })}
                        >
                          <IconTrash size={13} />
                        </button>
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
                  <button
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
                  </button>
                )}
                <div className="prov-form__actions-end">
                  {rightMode === "create" && providers[0] ? (
                    <button
                      type="button"
                      className="btn btn--ghost"
                      onClick={() => openEdit(providers[0]!)}
                      disabled={busy}
                    >
                      {tr("common.cancel")}
                    </button>
                  ) : null}
                  <button
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
                  </button>
                </div>
              </div>
            </div>
          )}
        </section>
      </div>

      <GlassModal
        open={modelPickerOpen}
        onClose={() => setModelPickerOpen(false)}
        title={tr("prov.modelPickerTitle")}
        size="md"
        closeLabel={tr("common.close")}
        footer={
          <>
            <button
              type="button"
              className="btn btn--ghost"
              onClick={() => setModelPickerOpen(false)}
            >
              {tr("common.cancel")}
            </button>
            <button
              type="button"
              className="btn btn--solid"
              onClick={applyRemoteModels}
              disabled={fetchingModels || selectedRemoteModels.size === 0}
            >
              {tr("prov.addSelected", { n: selectedRemoteModels.size })}
            </button>
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
              <label htmlFor="provider-remote-model-select-all">
                {selectedRemoteModels.size === remoteModels.length
                  ? tr("prov.deselectAll")
                  : tr("prov.selectAll")}
              </label>
              <span className="prov-model-picker__count">
                {selectedRemoteModels.size}/{remoteModels.length}
              </span>
            </div>
            <div className="prov-model-picker" role="list">
              {remoteModels.map((model) => (
                <div
                  className="prov-model-picker__row"
                  role="listitem"
                  key={model.id}
                >
                  <Checkbox
                    id={`provider-remote-model-${encodeURIComponent(model.id)}`}
                    className="size-[14px] cursor-pointer"
                    checked={selectedRemoteModels.has(model.id)}
                    aria-label={model.id}
                    onCheckedChange={() => toggleRemoteModel(model.id)}
                  />
                  <label
                    className="prov-model-picker__name"
                    htmlFor={`provider-remote-model-${encodeURIComponent(model.id)}`}
                  >
                    {model.id}
                  </label>
                  <span className="prov-model-picker__owner">
                    {model.ownedBy || ""}
                  </span>
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
            <button
              type="button"
              className="btn btn--ghost"
              onClick={() => setDeleteTarget(null)}
            >
              {tr("common.cancel")}
            </button>
            <button
              type="button"
              className="btn btn--danger"
              onClick={() => void confirmRemove()}
            >
              {tr("prov.delete")}
            </button>
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

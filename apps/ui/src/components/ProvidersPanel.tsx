import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Button } from "@appica/ui-react/button";
import { Alert, AlertAction, AlertDescription } from "@appica/ui-react/alert";
import { NumberField } from "@appica/ui-react/number-field";
import { Avatar, AvatarFallback } from "@appica/ui-react/avatar";
import { Field, FieldDescription, FieldLabel } from "@appica/ui-react/field";
/** 设置 → 模型设置：管理自定义模型供应商及其模型列表。 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "@/lib/api";
import { cachedRead, invalidateReadCache } from "@/lib/readCache";
import { copyTextInGesture } from "@/lib/clipboardWrite";
import { createT, type Locale } from "@/i18n";
import { formatTokenCount } from "@/lib/contextUsage";
import { localizeUiError } from "@/lib/session";
import {
  checkProviderImportText,
  providerExportFilename,
  type ProviderImportCheck,
} from "@/lib/providerTransfer";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Checkbox } from "@appica/ui-react/checkbox";
import { GlassModal } from "@/components/GlassModal";
import {
  IconCopy,
  IconDownload,
  IconEdit,
  IconPlus,
  IconPush,
  IconRefresh,
  IconTrash,
} from "@/components/icons";

export interface ProvidersPanelProps {
  locale: Locale;
  /** 供应商配置变化后通知桌面外壳刷新模型列表。 */
  onProviderActivated?: () => void;
  /** 进入面板时预选中的供应商标识；不存在或为空时回退列表第一项。 */
  initialProviderId?: string | null;
}

type FormState = {
  name: string;
  baseUrl: string;
  models: string[];
  modelDraft: string;
  /** 手动添加模型的上下文窗口输入（token）；未填写时保存流程使用 200K 默认值。 */
  contextWindowDraft: string;
  maxOutputTokensDraft: string;
  /** 手动添加模型是否支持图片输入。 */
  supportsVisionDraft: boolean;
  apiKey: string;
  apiBackend: string;
  chatOutputTokenField: "max_completion_tokens" | "max_tokens";
  /** 每模型手工配置的上下文窗口（token）；缺省表示自动获取或回退默认。 */
  contextWindows: Record<string, number>;
  maxOutputTokens: Record<string, number>;
  /** 每模型是否支持图片输入。 */
  supportsVision: Record<string, boolean>;
};

type RightMode = "empty" | "create" | "edit";

/** 导入弹窗的当前待导入文件状态；null 表示关闭。 */
type ImportDraft = {
  /** 用户选择的文件文本。 */
  text: string;
  /** 前端结构预检结果；error 时禁用提交。 */
  check: ProviderImportCheck;
  /** 提交进行中标记。 */
  submitting: boolean;
};

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
  contextWindowDraft: String(DEFAULT_MODEL_CONTEXT_WINDOW),
  maxOutputTokensDraft: "128000",
  supportsVisionDraft: false,
  apiKey: "",
  apiBackend: "responses",
  chatOutputTokenField: "max_completion_tokens",
  contextWindows: {},
  maxOutputTokens: {},
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

/** 模型上下文窗口默认值：200K。 */
const DEFAULT_MODEL_CONTEXT_WINDOW = 200_000;

/** 与后端 MAX_CONTEXT_WINDOW 一致：超出该范围的接口值不可保存。 */
const MAX_MODEL_CONTEXT_WINDOW = 10_000_000;

/** 接口窗口在合法范围内（<=10M）时采用接口值，否则回退默认 200K。 */
function effectiveContextWindow(value?: number | null): number {
  return value && value <= MAX_MODEL_CONTEXT_WINDOW
    ? value
    : DEFAULT_MODEL_CONTEXT_WINDOW;
}

/** 模型设置主面板。 */
export function ProvidersPanel({
  locale,
  onProviderActivated,
  initialProviderId = null,
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
      contextWindowDraft: String(DEFAULT_MODEL_CONTEXT_WINDOW),
      maxOutputTokensDraft: "128000",
      supportsVisionDraft: false,
      apiKey: provider.apiKey ?? "",
      apiBackend: provider.apiBackend,
      chatOutputTokenField: provider.chatOutputTokenField ?? "max_completion_tokens",
      contextWindows: { ...provider.contextWindows },
      maxOutputTokens: { ...provider.maxOutputTokens },
      supportsVision: { ...provider.supportsVision },
    });
    setHint(null);
    setShowKey(false);
    setRightMode("edit");
  }, []);

  /** 读取供应商列表；优先选中预选供应商，缺省时回退第一项。 */
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
      // 走短时缓存：切回本分区时立即复用上次结果，避免"每次都要重新加载"；
      // 供应商增删改会显式失效（见各写操作后的 invalidateReadCache）。
      const result = await cachedRead("providers_list", () => api.providersList());
      setList(result);
      const preferred = initialProviderId
        ? result.providers.find((provider) => provider.id === initialProviderId)
        : undefined;
      if (preferred ?? result.providers[0]) {
        openEdit(preferred ?? result.providers[0]);
      }
    } catch (loadError) {
      setError(localizeUiError(loadError, locale));
    } finally {
      setLoading(false);
    }
  }, [openEdit, initialProviderId]);

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
          ...(!draftEdited.current.has("context") ? { contextWindowDraft: String(effectiveContextWindow(item?.contextWindow)) } : {}),
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
      contextWindowDraft: String(DEFAULT_MODEL_CONTEXT_WINDOW),
      maxOutputTokensDraft: "128000",
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
      contextWindowDraft: remote
        ? String(effectiveContextWindow(item?.contextWindow))
        : String(current.contextWindows[model] ?? DEFAULT_MODEL_CONTEXT_WINDOW),
      maxOutputTokensDraft: String(remote ? item?.maxOutputTokens ?? 128000 : current.maxOutputTokens[model] ?? 128000),
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
        contextWindow: Number(form.contextWindowDraft) || null,
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
        contextWindowDraft: String(DEFAULT_MODEL_CONTEXT_WINDOW),
      maxOutputTokensDraft: "128000",
        supportsVisionDraft: false,
        contextWindows,
        maxOutputTokens: { ...current.maxOutputTokens, [model]: Number(current.maxOutputTokensDraft) },
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
      const supportsVision = { ...current.supportsVision };
      delete supportsVision[model];
      return {
        ...current,
        models: current.models.filter((item) => item !== model),
        contextWindows,
        maxOutputTokens,
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
        contextWindows: form.contextWindows,
        maxOutputTokens: Object.fromEntries(form.models.map((model) => [model, form.maxOutputTokens[model] ?? 128000])),
        supportsVision: form.supportsVision,
        createOnly: !editingId,
      });
      invalidateReadCache("providers_list");
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
      invalidateReadCache("providers_list");
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
        contextWindow: form.contextWindows[model.id] ?? effectiveContextWindow(metadata.get(model.id)?.contextWindow ?? model.contextWindow),
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

  /** 导入弹窗的当前待导入文件状态；null 表示关闭。 */
  const [importDraft, setImportDraft] = useState<ImportDraft | null>(null);

  /** 导入弹窗的前端结构预检错误本地化文案。 */
  const importErrorLabel = (error: "json" | "schema" | "empty") =>
    error === "schema"
      ? tr("prov.importErr.schema")
      : error === "empty"
        ? tr("prov.importErr.empty")
        : tr("prov.importErr.json");

  /** 把 JSON 文本作为下载文件保存；与会话导出共用浏览器下载通道。 */
  const downloadJson = (text: string, filename: string) => {
    const blob = new Blob([text], { type: "application/json;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = filename;
    anchor.click();
    URL.revokeObjectURL(url);
  };

  /**
   * 以 JSON 复制当前编辑供应商的完整配置（含 API Key）。
   * 导出的 IPC 往返在写入发起后完成，避免等待期间手势过期被 WebKit 拒绝。
   */
  const copyProvider = async (provider: api.CustomProvider) => {
    try {
      await copyTextInGesture(() => api.providersExport(provider.id));
      setHint(tr("prov.copyDone"));
      setHintTone("ok");
    } catch (copyError) {
      setHint(tr("prov.copyFail", { error: localizeUiError(copyError, locale) }));
      setHintTone("err");
    }
  };

  /** 导出当前编辑供应商的完整配置为 JSON 文件。 */
  const exportProvider = async (provider: api.CustomProvider) => {
    try {
      const json = await api.providersExport(provider.id);
      downloadJson(json, providerExportFilename(provider.name || provider.id));
      setHint(tr("prov.exportDone"));
      setHintTone("ok");
    } catch (exportError) {
      setHint(tr("prov.exportFail", { error: localizeUiError(exportError, locale) }));
      setHintTone("err");
    }
  };

  /** 打开系统文件选择器并读取待导入 JSON。 */
  const pickImportFile = async () => {
    if (!api.isTauri() || importDraft?.submitting) return;
    try {
      const text = await api.pickTextFile();
      if (text == null) return;
      setImportDraft({
        text,
        check: checkProviderImportText(text),
        submitting: false,
      });
    } catch (pickError) {
      setHint(localizeUiError(pickError, locale));
      setHintTone("err");
    }
  };

  /** 提交导入：后端按标识合并并热加载，完成后刷新本地列表。 */
  const submitImport = async () => {
    if (!importDraft || importDraft.submitting) return;
    if (!importDraft.check.ok) return;
    setImportDraft({ ...importDraft, submitting: true });
    try {
      const result = await api.providersImport(importDraft.text);
      invalidateReadCache("providers_list");
      setList({
        providers: result.providers,
        defaultModel: result.defaultModel,
        activeProviderId: result.activeProviderId,
      });
      setImportDraft(null);
      setHint(
        tr("prov.importDone", {
          added: result.added,
          updated: result.updated,
        }),
      );
      setHintTone("ok");
      onProviderActivated?.();
    } catch (importError) {
      setImportDraft((current) =>
        current ? { ...current, submitting: false } : current,
      );
      setHint(tr("prov.importFail", { error: localizeUiError(importError, locale) }));
      setHintTone("err");
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
      const supportsVision = { ...current.supportsVision };
      const maxOutputTokens = { ...current.maxOutputTokens };
      for (const model of remoteModels.filter((item) => selectedRemoteModels.has(item.id))) {
        if (model.contextWindow) contextWindows[model.id] = model.contextWindow;
        else delete contextWindows[model.id];
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
        <Alert variant="error" layout="inline">
          <AlertDescription>{error}</AlertDescription>
          <AlertAction><Button type="button" variant="ghost" size="md" onClick={() => setError(null)}>{tr("common.dismiss")}</Button></AlertAction>
        </Alert>
      )}

      <div className="prov-split">
        <aside className="prov-split__list">
          <Button size="md"
            type="button"
            variant="primary" className="prov-add-btn"
            onClick={openCreate}
            disabled={busy}
          >
            <IconPlus size={16} />
            {tr("prov.new")}
          </Button>
          {/* 表单未展示时（空态）提示只能落在左栏。 */}
          {rightMode === "empty" && hint ? (
            <div
              className={
                "prov-form__hint" +
                (hintTone === "ok" ? " is-ok" : hintTone === "err" ? " is-err" : "")
              }
            >
              {hint}
            </div>
          ) : null}

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
            <Button size="md"
              type="button"
              variant="ghost"
              className="prov-item__main"
                  onClick={() => openEdit(provider)}
                >
                  <Avatar size="md" aria-hidden><AvatarFallback>{(provider.name || provider.id).slice(0, 1).toUpperCase()}</AvatarFallback></Avatar>
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
            <Card
              className="prov-detail prov-form"
              contentProps={{ className: "prov-detail__content" }}
              data-testid="provider-form"
              inset={false}
            >
              <div className="prov-form__head">
                <h3 className="prov-detail__title">
                  {editingId ? tr("prov.editTitle") : tr("prov.addTitle")}
                </h3>
                {/* 右上角操作随模式切换：新增态提供导入入口，编辑态为当前供应商的复制与导出。 */}
                <span className="prov-transfer-row">
                  {editingId ? (
                    <>
                      <Button
                        type="button"
                        variant="ghost" size="md"
                        onClick={() => {
                          const provider = providers.find((item) => item.id === editingId);
                          if (provider) void copyProvider(provider);
                        }}
                        disabled={busy}
                      >
                        <IconCopy size={14} />
                        {tr("prov.copy")}
                      </Button>
                      <Button
                        type="button"
                        variant="ghost" size="md"
                        onClick={() => {
                          const provider = providers.find((item) => item.id === editingId);
                          if (provider) void exportProvider(provider);
                        }}
                        disabled={busy}
                      >
                        <IconDownload size={14} />
                        {tr("prov.exportOne")}
                      </Button>
                    </>
                  ) : (
                    <Button
                      type="button"
                      variant="ghost" size="md"
                      onClick={() =>
                        setImportDraft({
                          text: "",
                          check: { ok: false, error: "json" },
                          submitting: false,
                        })
                      }
                      disabled={busy}
                    >
                      <IconPush size={14} />
                      {tr("prov.importAll")}
                    </Button>
                  )}
                </span>
              </div>

              <div className="prov-form__grid">
                <Field>
                  <FieldLabel>{tr("prov.name")}</FieldLabel>
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
                </Field>

                <Field className="prov-field--full">
                  <FieldLabel>{tr("prov.baseUrl")}</FieldLabel>
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
                </Field>

                <Field>
                  <FieldLabel>{tr("prov.protocol")}</FieldLabel>
                  <Select
                    value={form.apiBackend}
                    onValueChange={(value) => {
                      if (typeof value !== "string") return;
                      setForm((current) => ({
                        ...current,
                        apiBackend: value,
                      }));
                    }}
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
                </Field>

                {form.apiBackend === "chat_completions" && (
                  <Field>
                    <FieldLabel>{tr("prov.chatOutputTokenField")}</FieldLabel>
                    <Select value={form.chatOutputTokenField} onValueChange={(value) => setForm((current) => ({ ...current, chatOutputTokenField: value as FormState["chatOutputTokenField"] }))}>
                      <SelectTrigger className="settings-input" aria-label={tr("prov.chatOutputTokenField")}><SelectValue /></SelectTrigger>
                      <SelectContent><SelectGroup>
                        <SelectItem value="max_completion_tokens">max_completion_tokens</SelectItem>
                        <SelectItem value="max_tokens">max_tokens</SelectItem>
                      </SelectGroup></SelectContent>
                    </Select>
                  </Field>
                )}

                <Field className="prov-field--full">
                  <FieldLabel>{tr("prov.apiKey")}</FieldLabel>
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
                      variant="ghost" size="md"
                      onClick={toggleKeyVisibility}
                    >
                      {showKey ? tr("prov.keyHide") : tr("prov.keyShow")}
                    </Button>
                  </div>
                  <FieldDescription>
                    {tr("prov.keyStorageHint")}
                  </FieldDescription>
                </Field>

                <Field className="prov-field--full">
                  <div className="prov-field__label-row prov-model-toolbar">
                    <FieldLabel>
                      {tr("prov.modelList")}
                    </FieldLabel>
                    <span className="prov-model-actions">
                      <Button
                        type="button"
                        variant="ghost"
                        size="md"
                        className={fetchingModels ? "prov-fetch-button is-loading" : "prov-fetch-button"}
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
                        variant="ghost" size="md"
                        onClick={openAddModel}
                        disabled={busy}
                      >
                        <IconPlus size={14} />
                        {tr("prov.addModel")}
                      </Button>
                    </span>
                  </div>
                  <div className="prov-model-list" role="list">
                    {form.models.map((model) => (
                      <div className="prov-model-row" role="listitem" key={model}>
                        <span className="prov-model-row__name" title={model}>
                          {model}
                        </span>
                        {form.supportsVision[model] && <span>{tr("prov.supportsVision")}</span>}
                        {form.contextWindows[model] && (
                          <span title={tr("prov.contextWindowFor", { model })}>
                            {formatTokenCount(form.contextWindows[model])}
                          </span>
                        )}
                        <Button type="button" variant="ghost" size="icon-md" className="tree-icon-btn"
                          aria-label={`${tr("prov.editModel")} ${model}`}
                          onClick={() => openModelEditor(model)}>
                          <IconEdit size={14} />
                        </Button>
                        <Button
                          type="button"
                          variant="ghost" size="icon-md" className="tree-icon-btn"
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
                </Field>
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
                  <Button size="md"
                    type="button"
                    variant="destructive"
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
                    <Button size="md"
                      type="button"
                      variant="ghost"
                      onClick={() => openEdit(providers[0]!)}
                      disabled={busy}
                    >
                      {tr("common.cancel")}
                    </Button>
                  ) : null}
                  <Button size="md"
                    type="button"
                    variant="primary"
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
            </Card>
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
            <Button size="md"
              type="button"
              variant="ghost"
              onClick={closeModelEditor}
            >
              {tr("common.cancel")}
            </Button>
            <Button size="md"
              type="submit"
              variant="primary"
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
          <Field>
            <FieldLabel htmlFor="provider-model-name-draft">{tr("prov.modelId")}</FieldLabel>
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
                  contextWindowDraft: String(DEFAULT_MODEL_CONTEXT_WINDOW),
                  supportsVisionDraft: false,
                  maxOutputTokensDraft: "128000",
                }));
              }}
              placeholder={tr("prov.modelPh")}
              autoComplete="off"
              spellCheck={false}
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="provider-model-context-draft">{tr("prov.contextWindow")}</FieldLabel>
            <NumberField
              size="md"
              id="provider-model-context-draft"
              min={1024}
              max={10000000}
              step="any"
              required
              inputProps={{ inputMode: "numeric" }}
              value={form.contextWindowDraft ? Number(form.contextWindowDraft) : null}
              onValueChange={(value) => { draftEdited.current.add("context"); setForm((current) => ({ ...current, contextWindowDraft: value == null ? "" : String(value) })); }}
              placeholder={tr("prov.contextWindowPh")}
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="provider-model-output-draft">{tr("prov.maxOutputTokens")}</FieldLabel>
            <NumberField size="md" id="provider-model-output-draft" min={1} max={4294967295} step={1} required
              value={form.maxOutputTokensDraft ? Number(form.maxOutputTokensDraft) : null}
              onValueChange={(value) => { draftEdited.current.add("output"); setForm((current) => ({ ...current, maxOutputTokensDraft: value == null ? "" : String(value) })); }} />
          </Field>
          <div className="prov-model-options">
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
              <label htmlFor="provider-model-vision-draft">{tr("prov.supportsVision")}</label>
            </div>
          </div>
          <FieldDescription>{modelEditTarget ? tr("prov.modelEditHint") : tr("prov.modelAddHint")}</FieldDescription>
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
            <Button size="md"
              type="button"
              variant="ghost"
              onClick={() => setModelPickerOpen(false)}
            >
              {tr("common.cancel")}
            </Button>
            <Button size="md"
              type="button"
              variant="primary"
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
                  <label
                    className="prov-model-picker__name"
                    htmlFor={`provider-remote-model-${encodeURIComponent(model.id)}`}
                  >
                    {model.id}
                  </label>
                  <div className="flex items-center justify-end gap-2">
                    {model.supportsVision && <span>{tr("prov.supportsVision")}</span>}
                    {model.contextWindow != null && <span title={tr("prov.contextWindowFor", { model: model.id })}>{formatTokenCount(model.contextWindow)}</span>}
                    <Button type="button" variant="ghost" size="icon-md" className="tree-icon-btn"
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
        open={importDraft !== null}
        onClose={() => {
          if (!importDraft?.submitting) setImportDraft(null);
        }}
        title={tr("prov.importTitle")}
        size="md"
        closeLabel={tr("common.close")}
        wrapBody
        footer={
          <>
            <Button size="md"
              type="button"
              variant="ghost"
              disabled={importDraft?.submitting}
              onClick={() => setImportDraft(null)}
            >
              {tr("common.cancel")}
            </Button>
            <Button size="md"
              type="button"
              variant="primary"
              disabled={!importDraft || !importDraft.check.ok || importDraft.submitting}
              onClick={() => void submitImport()}
            >
              {importDraft?.submitting
                ? tr("prov.importWorking")
                : tr("prov.importSubmit")}
            </Button>
          </>
        }
      >
        <p className="prov-field__hint">{tr("prov.importHint")}</p>
        <div className="prov-transfer-row">
          <Button size="md"
            type="button"
            variant="ghost"
            onClick={() => void pickImportFile()}
            disabled={importDraft?.submitting}
          >
            {tr("prov.importPickFile")}
          </Button>
          <span className="prov-field__hint">{tr("prov.importPickHint")}</span>
        </div>
        {importDraft && importDraft.text ? (
          importDraft.check.ok ? (
            <p className="prov-form__hint is-ok">
              {tr("prov.importParsed", { n: importDraft.check.count })}
            </p>
          ) : (
            <Alert variant="error"><AlertDescription>{importErrorLabel(importDraft.check.error)}</AlertDescription></Alert>
          )
        ) : null}
      </GlassModal>

      <GlassModal
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        title={tr("prov.delete")}
        size="md"
        closeLabel={tr("common.close")}
        footer={
          <>
            <Button size="md"
              type="button"
              variant="ghost"
              onClick={() => setDeleteTarget(null)}
            >
              {tr("common.cancel")}
            </Button>
            <Button size="md"
              type="button"
              variant="destructive"
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

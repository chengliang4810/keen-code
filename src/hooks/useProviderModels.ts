import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import * as api from "@/lib/api";
import { diagnosticsRecord } from "@/lib/acp/api";
import { localizeUiError } from "@/lib/session";
import {
  applyModelMetadata,
  DEFAULT_EFFORT,
  effortsForModel,
  findActiveModel,
  formatSessionModelReference,
  hasConfiguredProviderModel,
  isBoundSessionModelReference,
  isValidEffort,
  isValidModelId,
  modelIdFromSessionReference,
  pickDefaultEffort,
  pickNewChatModel,
  providerIdFromSessionReference,
  type ModelOption,
} from "@/lib/modelCatalog";

type SetState<T> = Dispatch<SetStateAction<T>>;

export type ConfiguredModelsRef = MutableRefObject<ModelOption[]>;

export type ShowToast = (message: string, durationMs?: number) => void;

export interface UseProviderModelsOptions {
  /** 当前工作台正在查看的会话；空值表示新会话草稿。 */
  sessionId: string | null;
  /** 当前界面语言，用于模型切换失败提示和供应商切换提示。 */
  locale: Locale;
  showToast: ShowToast;
}

export interface UseProviderModelsResult {
  /** 当前会话或草稿使用的模型 ID。 */
  modelId: string;
  /** 当前会话实际绑定的供应商；草稿尚未选定或引用缺失时为空。 */
  sessionProviderId: string | null;
  /**
   * 以 `providerId::modelId` 引用更新 Composer 模型。
   *
   * 同一模型 ID 可能同时存在于多个供应商，只保存模型 ID 会让菜单回落成全局活跃
   * 供应商，因此这里保存完整引用；空引用表示尚未选定。
   */
  setSessionModelReference: SetState<string>;
  effort: string;
  setEffort: SetState<string>;
  configuredModels: ModelOption[];
  configuredModelsRef: ConfiguredModelsRef;
  modelMetadataById: Record<string, api.ModelMetadata>;
  availableModels: ModelOption[];
  activeModel: ModelOption | undefined;
  modelLabel: string;
  activeCustomProvider: api.CustomProvider | null;
  activeCustomModelId: string | null;
  providerRouteRevision: number;
  setProviderRouteRevision: SetState<number>;
  refreshProviderRoute: () => Promise<void>;
  handleProviderActivated: () => void;
  hasConfiguredModel: boolean;
  isValidEffort: typeof isValidEffort;
  isValidModelId: typeof isValidModelId;
}

/** 管理自定义供应商路由、模型目录、模型元数据和会话级模型选择。 */
export function useProviderModels({
  sessionId,
  locale,
  showToast,
}: UseProviderModelsOptions): UseProviderModelsResult {
  const tr = useMemo(() => createT(locale), [locale]);
  /**
   * 会话模型引用 `providerId::modelId`。同一模型 ID 可能同时存在于多个供应商，
   * 只保留模型 ID 会让菜单回落成全局活跃供应商，因此这里保存完整引用。
   */
  const [sessionModelReference, setSessionModelReference] = useState("");
  /**
   * Host 在 Session 尚未绑定 Provider 时返回 `unconfigured` 占位值。该状态由运行时
   * 回退到全局默认 Provider，因此必须按“未选择”处理，不能当成模型名展示或据它拦截发送。
   */
  const boundSessionModelReference = isBoundSessionModelReference(
    sessionModelReference,
  )
    ? sessionModelReference
    : "";
  const modelId = modelIdFromSessionReference(boundSessionModelReference);
  const sessionProviderId = providerIdFromSessionReference(
    boundSessionModelReference,
  );
  /** 异步目录刷新只初始化新草稿，不能覆盖已恢复的会话模型。 */
  const currentSessionIdRef = useRef(sessionId);
  currentSessionIdRef.current = sessionId;
  const [effort, setEffort] = useState(DEFAULT_EFFORT);
  const [configuredModels, setConfiguredModels] = useState<ModelOption[]>([]);
  const configuredModelsRef = useRef<ModelOption[]>([]);
  configuredModelsRef.current = configuredModels;
  const [modelMetadataById, setModelMetadataById] = useState<
    Record<string, api.ModelMetadata>
  >({});
  const [activeCustomProvider, setActiveCustomProvider] =
    useState<api.CustomProvider | null>(null);
  const [activeCustomModelId, setActiveCustomModelId] = useState<string | null>(
    null,
  );
  const [providerRouteRevision, setProviderRouteRevision] = useState(0);

  /** 将按需元数据投影到模型菜单；供应商手工上下文配置优先。 */
  const availableModels = useMemo(
    () =>
      configuredModels.map((model) => {
        const merged = applyModelMetadata(model, modelMetadataById[model.id]);
        return {
          ...merged,
          contextWindow: model.contextWindow ?? merged.contextWindow,
          supportsVision: model.supportsVision ?? merged.supportsVision,
        };
      }),
    [configuredModels, modelMetadataById],
  );

  const activeModel = useMemo(
    () => findActiveModel(modelId, sessionProviderId, availableModels),
    [availableModels, modelId, sessionProviderId],
  );
  const modelLabel = activeModel?.label ?? modelId;

  const refreshProviderRoute = useCallback(async () => {
    if (!api.isTauri()) {
      setActiveCustomProvider(null);
      setActiveCustomModelId(null);
      setConfiguredModels([]);
      return;
    }
    try {
      const list = await api.providersList();
      const active =
        list.providers.find((provider) => provider.id === list.activeProviderId) ??
        null;
      setActiveCustomProvider(active);
      setActiveCustomModelId(list.defaultModel);
      const providerModels = list.providers.flatMap<ModelOption>((provider) =>
        provider.models.map((model) => ({
          providerId: provider.id,
          providerLabel: provider.name.trim() || provider.id,
          id: model,
          label: model,
          isDefault:
            list.activeProviderId === provider.id &&
            list.defaultModel === model,
          source: provider.apiBackend,
          // 手工配置优先；未配置时留空，由模型元数据合并填充，
          // 不再无条件回退 1M（会把真实窗口降级）。
          contextWindow: provider.contextWindows?.[model],
          // 供应商视觉配置是权威值，模型菜单据此标注图片输入能力。
          supportsVision: provider.supportsVision[model],
        })),
      );
      setConfiguredModels(providerModels);
      const defaultModel = pickNewChatModel(
        list.activeProviderId,
        list.defaultModel,
        providerModels,
      );
      // 草稿的模型引用必须带上供应商，否则菜单会借用同名模型的其他供应商条目。
      if (!currentSessionIdRef.current) {
        setSessionModelReference(
          defaultModel
            ? formatSessionModelReference(defaultModel.providerId, defaultModel.id)
            : "",
        );
      }
    } catch {
      /* 保留上一次可用路由，避免设置页短暂失败清空当前模型。 */
    }
  }, []);

  /** 仅按 modelId 查询固定公共目录，供应商名称和地址不参与匹配。 */
  useEffect(() => {
    if (!api.isTauri() || !modelId) return;
    let cancelled = false;
    void api
      .modelMetadataGet(modelId)
      .then((metadata) => {
        if (cancelled || metadata.modelId !== modelId) return;
        setModelMetadataById((current) => {
          if (current[modelId]?.updatedAt === metadata.updatedAt) return current;
          return { ...current, [modelId]: metadata };
        });
        const model = applyModelMetadata(
          { id: modelId, label: modelId },
          metadata,
        );
        const efforts = effortsForModel(model);
        if (efforts.length > 0) {
          setEffort((current) =>
            efforts.some((entry) => entry.id === current)
              ? current
              : pickDefaultEffort(model),
          );
        }
        if (
          !sessionId &&
          activeCustomProvider?.id &&
          activeCustomModelId === modelId
        ) {
          void api
            .providersSelectModel(activeCustomProvider.id, modelId)
            .catch((error: unknown) =>
              diagnosticsRecord(
                "frontend.model_context_window_reload",
                `${modelId}: ${String(error)}`,
              ),
            );
        }
      })
      .catch((error: unknown) => {
        void diagnosticsRecord(
          "frontend.model_metadata",
          `${modelId}: ${String(error)}`,
        ).catch(() => {});
      });
    return () => {
      cancelled = true;
    };
  }, [activeCustomModelId, activeCustomProvider?.id, modelId, sessionId]);

  useEffect(() => {
    void refreshProviderRoute();
  }, [refreshProviderRoute]);

  const handleProviderActivated = useCallback(() => {
    void refreshProviderRoute()
      .then(() => {
        setProviderRouteRevision((revision) => revision + 1);
        showToast(tr("prov.switchedHotReload"), 3200);
      })
      .catch((error: unknown) =>
        showToast(localizeUiError(error, locale), 4500),
      );
  }, [locale, refreshProviderRoute, showToast, tr]);

  // 发送门槛保持按全局默认供应商判断：会话绑定的供应商被删除时，运行时仍会给出
  // 明确错误，比在这里静默禁用发送按钮更容易定位。
  const hasConfiguredModel = hasConfiguredProviderModel(
    activeCustomProvider?.id,
    activeCustomModelId,
    availableModels,
  );

  return {
    modelId,
    sessionProviderId,
    setSessionModelReference,
    effort,
    setEffort,
    configuredModels,
    configuredModelsRef,
    modelMetadataById,
    availableModels,
    activeModel,
    modelLabel,
    activeCustomProvider,
    activeCustomModelId,
    providerRouteRevision,
    setProviderRouteRevision,
    refreshProviderRoute,
    handleProviderActivated,
    hasConfiguredModel,
    isValidEffort,
    isValidModelId,
  };
}

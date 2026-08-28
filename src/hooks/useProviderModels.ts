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
  hasConfiguredProviderModel,
  isValidEffort,
  isValidModelId,
  pickDefaultEffort,
  pickNewChatModel,
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
  modelId: string;
  setModelId: SetState<string>;
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
  const [modelId, setModelId] = useState("");
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
        if (model.contextWindow) {
          return { ...merged, contextWindow: model.contextWindow };
        }
        return merged;
      }),
    [configuredModels, modelMetadataById],
  );

  const activeModel = useMemo(
    () =>
      availableModels.find(
        (model) =>
          model.id === modelId &&
          (!activeCustomProvider?.id ||
            model.providerId === activeCustomProvider.id),
      ),
    [activeCustomProvider?.id, availableModels, modelId],
  );
  const modelLabel =
    availableModels.find((model) => model.id === modelId)?.label ?? modelId;

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
          // 1M 标志优先于手工上下文配置，均优先于公共元数据目录。
          contextWindow: provider.context1m?.[model]
            ? 1_000_000
            : provider.contextWindows?.[model],
        })),
      );
      setConfiguredModels(providerModels);
      const defaultModel = pickNewChatModel(
        list.activeProviderId,
        list.defaultModel,
        providerModels,
      );
      setModelId(defaultModel?.id ?? "");
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

  const hasConfiguredModel = hasConfiguredProviderModel(
    activeCustomProvider?.id,
    activeCustomModelId,
    availableModels,
  );

  return {
    modelId,
    setModelId,
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

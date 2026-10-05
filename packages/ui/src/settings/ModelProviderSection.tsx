import { useCallback, useEffect, useMemo, useState } from "react";
import type { ModelConnectivityResult } from "@zcode/shared";
import { useZCodeIntl } from "@/i18n/IntlProvider.js";
import { Button } from "@/components/ui/button.js";
import { useModelProviders } from "@/hooks/useModelProviders.js";
import type { ProviderSettingsFormProvider } from "@/lib/providerSettingsFormTypes.js";
import { ModelProviderSectionLayout } from "./model-provider-section/SectionLayout.js";
import { ProviderTemplatePicker } from "./model-provider-section/ProviderTemplatePicker.js";
import { InlineEditableProviderCard } from "./model-provider-section/InlineEditableProviderCard.js";
import type { ModelProviderNavGroup } from "./model-provider-section/constants.js";
import {
  fuzzyMatch,
  handleEndpointSuggestionPopoverOpenAutoFocus,
  resolveEndpointSuggestionOpenRequest,
  createCustomProviderNodeKey,
} from "./model-provider-section/utils.js";
import type { SettingsModelProviderTarget } from "@/lib/settingsNavigation.js";
import {
  addPendingSettingsSectionListener,
} from "@/lib/settingsNavigation.js";

export {
  fuzzyMatch,
  handleEndpointSuggestionPopoverOpenAutoFocus,
  resolveEndpointSuggestionOpenRequest,
} from "./model-provider-section/utils.js";

function providerNodeKey(provider: ProviderSettingsFormProvider): string {
  return createCustomProviderNodeKey(provider.providerId);
}

function providerNavigationItem(provider: ProviderSettingsFormProvider): ModelProviderNavGroup["items"][number] {
  return {
    key: providerNodeKey(provider),
    type: "custom",
    label: provider.providerName || provider.providerId,
    provider,
    statusActive: provider.enabled,
  };
}

/**
 * Provider 设置只保留本地配置与模型编辑。账号登录、套餐权益和购买面板不属于
 * KeenCode 的本地模型链路，移除后仍复用 ZCode 原有的 Provider 表单与布局。
 */
export function ModelProviderSection({
  workspacePath = "",
  connectivityWorkspacePath,
  connectivityWorkspaceRequired = false,
  pendingModelProviderTarget,
  onConsumePendingModelProviderTarget,
}: {
  workspacePath?: string;
  connectivityWorkspacePath?: string;
  connectivityWorkspaceRequired?: boolean;
  pendingModelProviderTarget?: SettingsModelProviderTarget;
  onConsumePendingModelProviderTarget?: () => void;
} = {}) {
  const { intl } = useZCodeIntl();
  const {
    modelProviders,
    providerTemplates,
    displayOrder,
    loading,
    loadError,
    reload,
    refreshing,
    refresh,
    saveProvider,
    createPersonalProvider,
    addPersonalModel,
    savePersonalModelDraft,
    setPersonalModelEnabled,
    deletePersonalModel,
    deleteProvider,
    reorderProviderModels,
    saveDisplayOrder,
    reorderableProviderIds,
    testModelConnectivity,
  } = useModelProviders({
    workspacePath,
    connectivityWorkspacePath,
    connectivityWorkspaceRequired,
    connectivityUnavailableMessage: intl.formatMessage({
      id: "settings.modelProvider.testModel.localWorkspaceUnavailable",
    }),
  });
  const navigationGroups = useMemo<ModelProviderNavGroup[]>(() => {
    const providers = [...modelProviders].sort((left, right) => {
      const leftIndex = displayOrder.providerIds.indexOf(left.providerId);
      const rightIndex = displayOrder.providerIds.indexOf(right.providerId);
      return (leftIndex < 0 ? Number.MAX_SAFE_INTEGER : leftIndex) -
        (rightIndex < 0 ? Number.MAX_SAFE_INTEGER : rightIndex);
    });
    return [
      {
        id: "custom",
        // 只有自定义供应商，直接展示列表，不再制造空的内置分组或重复小标题。
        title: "",
        items: providers.map(providerNavigationItem),
      },
    ];
  }, [displayOrder, modelProviders]);
  const allNavigationItems = useMemo(
    () => navigationGroups.flatMap((group) => group.items),
    [navigationGroups],
  );
  const [selectedNodeKey, setSelectedNodeKey] = useState<string | null>(null);
  const [templatePickerOpen, setTemplatePickerOpen] = useState(false);
  const [creatingProvider, setCreatingProvider] = useState(false);

  useEffect(() => {
    if (pendingModelProviderTarget?.providerId) {
      const target = modelProviders.find((provider) => provider.providerId === pendingModelProviderTarget.providerId);
      if (target) setSelectedNodeKey(providerNodeKey(target));
      onConsumePendingModelProviderTarget?.();
      return;
    }
    if (!selectedNodeKey || !allNavigationItems.some((item) => item.key === selectedNodeKey)) {
      setSelectedNodeKey(allNavigationItems[0]?.key ?? null);
    }
  }, [allNavigationItems, modelProviders, onConsumePendingModelProviderTarget, pendingModelProviderTarget, selectedNodeKey]);

  useEffect(
    () =>
      addPendingSettingsSectionListener((section, detail) => {
        if (section !== "modelProvider" || !detail?.modelProviderId) return;
        const target = modelProviders.find((provider) => provider.providerId === detail.modelProviderId);
        if (target) setSelectedNodeKey(providerNodeKey(target));
      }),
    [modelProviders],
  );

  const selectedProvider = useMemo(() => {
    const item = allNavigationItems.find((candidate) => candidate.key === selectedNodeKey);
    return item && "provider" in item ? item.provider : null;
  }, [allNavigationItems, selectedNodeKey]);

  const handleCreateProvider = useCallback(
    async (input: { templateId?: string; providerName?: string }) => {
      setCreatingProvider(true);
      try {
        const created = await createPersonalProvider(input as Parameters<typeof createPersonalProvider>[0]);
        const providerId = (created as { providerId?: string }).providerId;
        if (providerId) setSelectedNodeKey(createCustomProviderNodeKey(providerId));
        setTemplatePickerOpen(false);
      } finally {
        setCreatingProvider(false);
      }
    },
    [createPersonalProvider],
  );

  const handleSave = useCallback(async (provider: ProviderSettingsFormProvider) => {
    await saveProvider(provider);
  }, [saveProvider]);
  const handleDelete = useCallback(async () => {
    if (!selectedProvider) return;
    await deleteProvider(selectedProvider.providerId);
    setSelectedNodeKey(null);
  }, [deleteProvider, selectedProvider]);
  const handleTestModel = useCallback(
    (providerId: string, modelId: string): Promise<ModelConnectivityResult> =>
      testModelConnectivity(providerId, modelId),
    [testModelConnectivity],
  );
  const handleReorderProviderIds = useCallback(
    (providerIds: string[]) => saveDisplayOrder({ providerIds }),
    [saveDisplayOrder],
  );
  const handleRefresh = useCallback(() => void refresh(), [refresh]);

  if (loadError) {
    return (
      <div className="flex min-h-64 flex-col items-center justify-center gap-3 text-ui-base">
        <p className="text-destructive">{loadError.message}</p>
        <Button type="button" variant="outline" onClick={reload}>
          {intl.formatMessage({ id: "common.retry" })}
        </Button>
      </div>
    );
  }

  return (
    <ModelProviderSectionLayout
      description={intl.formatMessage({ id: "settings.modelProviderDescription" })}
      refreshLabel={intl.formatMessage({ id: "settings.modelProvider.refresh" })}
      loadingLabel={intl.formatMessage({ id: "common.loading" })}
      presetLoading={loading || refreshing}
      customLoading={loading || refreshing}
      onRefresh={handleRefresh}
      addProviderLabel={intl.formatMessage({ id: "settings.modelProvider.addProviderAction" })}
      onAddProvider={() => setTemplatePickerOpen(true)}
      navigationGroups={navigationGroups}
      selectedNodeKey={selectedNodeKey}
      onSelectNavItem={(item) => setSelectedNodeKey(item.key)}
      onReorderProviderIds={handleReorderProviderIds}
      reorderableProviderIds={reorderableProviderIds}
    >
      {templatePickerOpen ? (
        <ProviderTemplatePicker
          templates={providerTemplates}
          creating={creatingProvider}
          onBack={() => setTemplatePickerOpen(false)}
          onCreateFromTemplate={(templateId) => handleCreateProvider({ templateId })}
          onCreateCustom={(providerName) => handleCreateProvider({ providerName })}
        />
      ) : selectedProvider ? (
        <InlineEditableProviderCard
          provider={selectedProvider}
          onSave={handleSave}
          onAddPersonalModel={addPersonalModel}
          onSavePersonalModelDraft={savePersonalModelDraft}
          onSetPersonalModelEnabled={setPersonalModelEnabled}
          onDeletePersonalModel={deletePersonalModel}
          onDelete={() => void handleDelete()}
          onTestModel={handleTestModel}
          onReorderModelIds={(modelIds) => reorderProviderModels(selectedProvider.providerId, modelIds)}
          settingsRevision={selectedProvider.config ? 1 : 0}
        />
      ) : (
        <div className="flex min-h-64 items-center justify-center text-ui-base text-foreground-subtle">
          {intl.formatMessage({ id: "settings.modelProvider.empty" })}
        </div>
      )}
    </ModelProviderSectionLayout>
  );
}

export type { ProviderSettingsFormProvider };

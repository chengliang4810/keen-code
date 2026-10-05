/* oxlint-disable eslint(max-lines) -- 模型、思考深度和本地 context 用量保持同一工具条布局。 */
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { isApiKeyAccess } from "@zcode/provider";
import {
  TID_V4_MODEL_CONFIG,
  TID_V4_COMPOSER_INPUT,
  ZCODE_AGENT_PROVIDER,
  type ZCodeConfigOption,
  type ZCodeProvider,
} from "@zcode/shared";
import type {
  SessionConfigState,
  SessionPhase,
  SessionUsageState,
} from "@zcode/shared/zcode-protocol-v4";
import { ModelConfigSelect, type ModelSelectGroup } from "@/ModelConfigSelect.js";
import { Button } from "@/components/ui/button.js";
import { ChatContextUsage } from "@/chat-input-toolbar/display.js";
import { ThoughtLevelCycleControl } from "@/chat-input-toolbar/ThoughtLevelCycleControl.js";
import { getNextThoughtLevelValue } from "@/chat-input-toolbar/thoughtLevelOptions.js";
import type { V4ComposerConfigPicker } from "@/v4/composer/configPickerState.js";
import { useToolbarShortcutBindings } from "@/v4/composer/toolbarShortcuts.js";
import {
  resolveModelSelectTriggerDisplay,
  shouldShowManageModelsAction,
} from "@/chat-input-toolbar/modelSelection.js";
import { resolveV4ModelTriggerDisplay } from "@/v4/composer/modelTriggerDisplay.js";
import { setPendingSettingsSectionIntent } from "@/lib/settingsNavigation.js";
import { useTabStore } from "@/store/TabStoreProvider.js";
import type { ModelSelectionView } from "@zcode/services";
import type { ModelSelectionState } from "@/hooks/useModelSelectionView.js";
import { useToolbarConfigOptions } from "@/hooks/useZCodeConfig.js";
import { useZCodeIntl } from "@/i18n/IntlProvider.js";
import { useShortcutCommandLabel } from "@/shortcuts/useShortcutBindings.js";
import { logger } from "@/logger.js";
import { decodeCustomModelValue, encodeCustomModelValue } from "@/lib/zcodeCustomModelValue.js";
import { buildRegistryModelSelectGroups } from "@/lib/modelSelectionGroups.js";
import {
  resolveDraftDisplayedConfig,
  resolveDraftModelThoughtOption,
  resolveDraftThoughtCurrentValue,
} from "@/v4/composer/draftWorkspaceDefaults.js";

export { V4ComposerModeSwitch } from "@/v4/composer/V4ComposerModeControls.js";

const V4_COMPOSER_INPUT_SELECTOR = `[data-testid="${TID_V4_COMPOSER_INPUT}"]`;
const MODEL_SELECTION_LOADING_STATE: ModelSelectionState = { status: "loading" };

function noop(): void {}

export interface ModelSelectionSource {
  provider: string;
  model: string;
}

export interface V4ComposerToolbarProps {
  workspacePath: string;
  workspaceIdentity?: string;
  modelSelectionView?: ModelSelectionView | null;
  modelSelectionState?: ModelSelectionState;
  modelSelectionReload?: () => void;
  sessionId: string | null;
  phase: SessionPhase | null;
  provider?: ZCodeProvider;
  isMobileViewport?: boolean;
  draftMode?: boolean;
  draftConfig?: Partial<SessionConfigState>;
  usage: SessionUsageState | null;
  disabled: boolean;
  activeConfigPicker: V4ComposerConfigPicker | null;
  onConfigPickerOpenChange: (picker: V4ComposerConfigPicker, open: boolean) => void;
  onSelectModel: (
    provider: string,
    model: string,
    sourceModel: ModelSelectionSource | null,
  ) => void;
  onSelectThought: (thought: string, modelContext: { provider: string; model: string }) => void;
  onSwitchMode: (mode: string) => void;
  onRecoverCustomModelSelection?: (
    value: string,
    sourceModel: ModelSelectionSource | null,
  ) => Promise<void> | void;
  onSendCompressionCommand?: (command: string) => void;
}

function V4ComposerModelControlsImpl({
  workspacePath,
  workspaceIdentity,
  modelSelectionView = null,
  modelSelectionState = MODEL_SELECTION_LOADING_STATE,
  modelSelectionReload,
  provider,
  draftMode = false,
  draftConfig,
  usage,
  disabled,
  activeConfigPicker,
  onConfigPickerOpenChange,
  onSelectModel,
  onSelectThought,
  onSendCompressionCommand,
  onRecoverCustomModelSelection,
}: V4ComposerToolbarProps) {
  const { intl, locale } = useZCodeIntl();
  const displayProvider = provider ?? ZCODE_AGENT_PROVIDER;
  const { error: configOptionsError } = useToolbarConfigOptions(
    workspacePath,
    null,
    workspaceIdentity,
  );
  const openSettingsTab = useTabStore((state) => state.openSettingsTab);
  const modelTriggerRef = useRef<HTMLSpanElement | null>(null);
  const thoughtTriggerRef = useRef<HTMLSpanElement | null>(null);
  const [modelMenuOpenRequestKey, setModelMenuOpenRequestKey] = useState(0);
  const [recoveryPending, setRecoveryPending] = useState(false);
  const handleOpenModelMenuShortcut = useCallback(() => {
    setModelMenuOpenRequestKey((current) => current + 1);
  }, []);
  const handleModelPickerOpenChange = useCallback(
    (open: boolean) => onConfigPickerOpenChange("model", open),
    [onConfigPickerOpenChange],
  );
  const handleThoughtPickerOpenChange = useCallback(
    (open: boolean) => onConfigPickerOpenChange("thought", open),
    [onConfigPickerOpenChange],
  );
  const modelOption = modelSelectionView?.providers.some((item) => item.models.length > 0)
    ? {
        id: "model",
        name: "Model",
        category: "model",
        type: "select" as const,
        currentValue: "",
        options: [],
      }
    : undefined;
  const effectiveConfig = useMemo(
    () => resolveDraftDisplayedConfig(draftConfig ?? {}),
    [draftConfig],
  );

  useEffect(() => {
    if (!draftMode) return;
    logger.debug("[v4-toolbar] draft effectiveConfig changed", {
      provider: effectiveConfig?.provider ?? null,
      model: effectiveConfig?.model ?? null,
      thought: effectiveConfig?.thought ?? null,
      modelSelectionRevision: modelSelectionView?.revision ?? null,
    });
  }, [draftMode, effectiveConfig, modelSelectionView?.revision]);

  const modelSelectGroups = useMemo<ModelSelectGroup[]>(() => {
    if (!modelSelectionView) return [];
    return buildRegistryModelSelectGroups(displayProvider, modelSelectionView, {
      apiKeyLabel: intl.formatMessage({ id: "settings.modelProvider.apiKey" }),
      apiKeyBadgeLabel: intl.formatMessage({ id: "settings.modelProvider.connectionMode.apiKeyBadge" }),
      codingPlanLabel: intl.formatMessage({ id: "settings.modelProvider.connectionMode.codingPlan" }),
      codingPlanBadgeLabel: intl.formatMessage({ id: "settings.modelProvider.connectionMode.codingPlanBadge" }),
      startPlanLabel: intl.formatMessage({ id: "settings.modelProvider.connectionMode.startPlan" }),
      startPlanBadgeLabel: intl.formatMessage({ id: "settings.modelProvider.connectionMode.startPlanBadge" }),
      teamPlanBadgeLabel: intl.formatMessage({ id: "settings.modelProvider.connectionMode.teamPlanBadge" }),
      teamPlanFallbackLabel: intl.formatMessage({ id: "settings.modelProvider.connectionMode.teamPlan" }),
    });
  }, [displayProvider, intl, modelSelectionView]);

  const handleOpenModelProviderSettings = useCallback(() => {
    setPendingSettingsSectionIntent("modelProvider");
    openSettingsTab();
  }, [openSettingsTab]);
  const showManageModelsAction = shouldShowManageModelsAction(handleOpenModelProviderSettings);
  const manageModelsLabel = intl.formatMessage({ id: "chat.toolbar.model.manageModels" });
  const rawModelValue = useMemo(() => {
    if (!effectiveConfig?.model) return "";
    const providerExists = modelSelectionView?.providers.some(
      (candidate) => candidate.providerId === effectiveConfig.provider,
    );
    return providerExists
      ? encodeCustomModelValue(effectiveConfig.provider, effectiveConfig.model)
      : effectiveConfig.model;
  }, [effectiveConfig, modelSelectionView]);
  const triggerDisplay = useMemo(
    () =>
      resolveModelSelectTriggerDisplay(
        rawModelValue,
        modelSelectGroups,
        showManageModelsAction,
        manageModelsLabel,
      ),
    [manageModelsLabel, modelSelectGroups, rawModelValue, showManageModelsAction],
  );
  const normalizedModelValue = triggerDisplay.value ?? "";
  const modelTriggerDisplay = useMemo(() => {
    const fallbackLabel =
      triggerDisplay.placeholder ?? intl.formatMessage({ id: "chat.toolbar.model.label" });
    const providerName = modelSelectionView?.providers.find(
      (candidate) => candidate.providerId === effectiveConfig?.provider,
    )?.providerName;
    return resolveV4ModelTriggerDisplay({
      modelGroups: modelSelectGroups,
      normalizedValue: normalizedModelValue,
      fallbackLabel,
      providerId: effectiveConfig?.provider ?? undefined,
      providerName: providerName ?? undefined,
    });
  }, [effectiveConfig?.provider, intl, modelSelectionView, modelSelectGroups, normalizedModelValue, triggerDisplay.placeholder]);
  const handleModelValueChange = useCallback(
    (value: string) => {
      const decoded = decodeCustomModelValue(value);
      const sourceModel =
        effectiveConfig?.provider && effectiveConfig.model
          ? { provider: effectiveConfig.provider, model: effectiveConfig.model }
          : null;
      const selectedRegistryProvider = decoded?.providerId
        ? modelSelectionView?.providers.find((item) => item.providerId === decoded.providerId)
        : undefined;
      if (configOptionsError && decoded?.providerId && isApiKeyAccess(selectedRegistryProvider?.config.access) && onRecoverCustomModelSelection) {
        setRecoveryPending(true);
        void Promise.resolve(onRecoverCustomModelSelection(value, sourceModel))
          .catch((error) => logger.warn("[v4-toolbar] custom provider recovery failed", { error }))
          .finally(() => setRecoveryPending(false));
        return;
      }
      if (decoded) {
        onSelectModel(decoded.providerId, decoded.modelName ?? "", sourceModel);
        return;
      }
      const slashIndex = value.indexOf("/");
      onSelectModel(
        slashIndex > 0 ? value.slice(0, slashIndex) : "",
        slashIndex > 0 ? value.slice(slashIndex + 1) : value,
        sourceModel,
      );
    },
    [configOptionsError, effectiveConfig, modelSelectionView, onRecoverCustomModelSelection, onSelectModel],
  );
  const draftModelThoughtOption = useMemo(
    () =>
      effectiveConfig
        ? resolveDraftModelThoughtOption(
            effectiveConfig.provider,
            effectiveConfig.model,
            modelSelectionView,
          )
        : null,
    [effectiveConfig, modelSelectionView],
  );
  const thoughtOption = useMemo<ZCodeConfigOption | null>(() => {
    if (!effectiveConfig || !draftModelThoughtOption) return null;
    return {
      ...draftModelThoughtOption,
      currentValue: resolveDraftThoughtCurrentValue({
        thought: effectiveConfig.thought,
        thoughtLevels: draftModelThoughtOption.options?.map((option) => option.value) ?? [],
      }),
    };
  }, [draftModelThoughtOption, effectiveConfig]);
  const handleThoughtValueChange = useCallback(
    (value: string) => {
      if (!effectiveConfig || !value.trim()) return;
      onSelectThought(value, { provider: effectiveConfig.provider, model: effectiveConfig.model });
    },
    [effectiveConfig, onSelectThought],
  );
  const handleCycleThoughtLevel = useCallback(() => {
    if (!thoughtOption || thoughtOption.type !== "select" || !effectiveConfig) return;
    const nextValue = getNextThoughtLevelValue(thoughtOption);
    if (nextValue != null) {
      onSelectThought(nextValue, {
        provider: effectiveConfig.provider,
        model: effectiveConfig.model,
      });
    }
  }, [effectiveConfig, onSelectThought, thoughtOption]);
  const taskUsage = useMemo(() => {
    const contextWindow = usage?.contextWindow;
    if (!contextWindow) return null;
    return {
      used: contextWindow.usedTokens,
      size: contextWindow.maxTokens,
      ...(contextWindow.cache ? { cache: contextWindow.cache } : {}),
      ...(contextWindow.breakdown ? { breakdown: contextWindow.breakdown } : {}),
    };
  }, [usage?.contextWindow]);
  const modelShortcutLabel = useShortcutCommandLabel("openModelMenu");
  const thoughtShortcutLabel = useShortcutCommandLabel("cycleThoughtLevel");
  const isModelOptionLocked = useCallback(() => false, []);
  const modelMenuVisible = modelSelectGroups.length > 0 || showManageModelsAction;
  useToolbarShortcutBindings({
    hasAnyOption: Boolean(modelOption) || Boolean(thoughtOption),
    toolbarDisabled: disabled || recoveryPending,
    modelMenuDisabled: disabled || recoveryPending || !modelMenuVisible,
    modelOption,
    thoughtOption: thoughtOption ?? undefined,
    onOpenModelMenu: handleOpenModelMenuShortcut,
    onCycleThoughtLevel: handleCycleThoughtLevel,
    onCycleSessionMode: noop,
  });

  return (
    <>
      <span
        data-testid={TID_V4_MODEL_CONFIG}
        data-source={effectiveConfig || draftConfig?.mode ? "composer" : ""}
        data-provider={effectiveConfig?.provider ?? ""}
        data-model={effectiveConfig?.model ?? ""}
        data-thought={thoughtOption?.type === "select" ? String(thoughtOption.currentValue ?? "") : (effectiveConfig?.thought ?? "")}
        data-thought-levels={thoughtOption?.type === "select" ? (thoughtOption.options ?? []).map((option) => option.value).join(",") : ""}
        data-mode={draftConfig?.mode ?? ""}
        data-plan-enabled={draftConfig?.planEnabled ?? false}
        data-usage-used={usage?.contextWindow?.usedTokens ?? ""}
        data-usage-max={usage?.contextWindow?.maxTokens ?? ""}
        className="hidden"
      />
      <ChatContextUsage
        taskUsage={taskUsage}
        selectedProvider={displayProvider}
        intl={intl}
        locale={locale}
        onSendCompressionCommand={onSendCompressionCommand}
        compressionDisabled={disabled || recoveryPending}
      />
      {modelSelectionState.status === "error" && modelSelectionReload ? (
        <Button type="button" variant="ghost" size="sm" className="h-7 px-2 text-ui-sm text-destructive" onClick={modelSelectionReload}>
          {intl.formatMessage({ id: "chat.toolbar.model.loadFailedRetry" })}
        </Button>
      ) : modelSelectionState.status === "unavailable" ? (
        <span className="px-2 text-ui-sm text-foreground-subtle">
          {intl.formatMessage({ id: modelSelectionState.reason === "remote-waiting" ? "chat.toolbar.model.remoteWaiting" : "chat.toolbar.model.targetMissing" })}
        </span>
      ) : modelMenuVisible ? (
        <ModelConfigSelect
          modelGroups={modelSelectGroups}
          normalizedValue={normalizedModelValue}
          triggerLabel={modelTriggerDisplay.fullLabel}
          triggerLabelPrefix={modelTriggerDisplay.providerPrefix}
          triggerLabelValue={modelTriggerDisplay.modelLabel}
          triggerLabelPrefixClassName="composer-provider-prefix inline group-data-[composer-provider-compact=true]/toolbar:hidden"
          showManageModelsAction={showManageModelsAction}
          manageModelsLabel={manageModelsLabel}
          onManageModels={handleOpenModelProviderSettings}
          lockReasonMessage={intl.formatMessage({ id: "chat.toolbar.modelSwitch.lockedByRunningTask" })}
          isItemLocked={isModelOptionLocked}
          onValueChange={handleModelValueChange}
          disabled={disabled || recoveryPending || modelSelectionState.status !== "ready"}
          tooltipTitle={modelTriggerDisplay.fullLabel}
          shortcutLabel={modelShortcutLabel}
          triggerRef={modelTriggerRef}
          open={activeConfigPicker === "model"}
          onOpenChange={handleModelPickerOpenChange}
          openRequestKey={modelMenuOpenRequestKey}
          labelVisibilityClassName="hidden @sm/composer:inline-flex"
          indicatorClassName="block group-data-[composer-model-icon=true]/toolbar:hidden"
          triggerLabelClassName="block min-w-0 text-left group-data-[composer-model-icon=true]/toolbar:hidden [&>span]:max-w-full [&>span>span]:block [&>span>span]:truncate"
          triggerClassName="composer-model-trigger group-data-[composer-model-icon=true]/toolbar:size-7 group-data-[composer-model-icon=true]/toolbar:p-0 group-data-[composer-model-icon=true]/toolbar:gap-0 group-data-[composer-model-icon=true]/toolbar:justify-center"
          triggerIconClassName="hidden group-data-[composer-model-icon=true]/toolbar:inline-flex"
          focusSelectorOnClose={V4_COMPOSER_INPUT_SELECTOR}
          providerSubmenuClassName={undefined}
        />
      ) : null}
      {thoughtOption ? (
        <ThoughtLevelCycleControl
          composerCollapsePriority={3}
          labelVisibilityClassName="inline-flex"
          indicatorClassName="block"
          option={thoughtOption}
          onValueChange={handleThoughtValueChange}
          disabled={disabled || recoveryPending}
          intl={intl}
          provider={displayProvider}
          shortcutLabel={thoughtShortcutLabel}
          triggerRef={thoughtTriggerRef}
          open={activeConfigPicker === "thought"}
          onOpenChange={handleThoughtPickerOpenChange}
          restoreFocusSelector={V4_COMPOSER_INPUT_SELECTOR}
        />
      ) : null}
    </>
  );
}

export const V4ComposerModelControls = memo(V4ComposerModelControlsImpl);

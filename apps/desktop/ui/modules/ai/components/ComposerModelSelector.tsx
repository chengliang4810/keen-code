import { useModelCatalogStore } from "@/modules/ai/lib/modelCatalogState";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  endpointModels,
  endpointModelSelectionId,
  getCompatModelInfo,
  effectiveCustomModel,
  resolveEndpointModel,
} from "@/modules/ai/config";
import { useComposer } from "@/modules/ai/lib/composer";
import { isConfiguredCustomModel } from "@/modules/ai/lib/modelSelection";
import {
  reasoningLevelLabel,
  selectedReasoningLevel,
} from "@/modules/ai/lib/reasoning";
import { useChatStore } from "@/modules/ai/store/chatStore";
import { useTranslation } from "@/modules/i18n";
import { openSettingsWindow } from "@/modules/settings/openSettingsWindow";
import { usePreferencesStore } from "@/modules/settings/preferences";
import {
  ArrowDown01Icon,
  BrainIcon,
  Settings01Icon,
  Tick01Icon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { lazy, Suspense, useEffect, useState } from "react";

const EffortSlider = lazy(() => import("@/modules/ai/components/EffortSlider"));

const triggerClass =
  "h-6 min-w-0 gap-1 rounded-md px-1.5 text-ui-base text-muted-foreground";

export function ComposerModelSelector() {
  const tr = useTranslation();
  useModelCatalogStore((s) => s.revision);
  const composer = useComposer();
  const selected = useChatStore((s) => s.selectedModelId);
  const setSelected = useChatStore((s) => s.setSelectedModelId);
  const endpoints = usePreferencesStore((s) => s.customEndpoints);
  const groups = endpoints.flatMap((endpoint) => {
    const models = endpoint.baseURL.trim()
      ? endpointModels(endpoint).filter(
          (model) => model.enabled !== false && model.id.trim(),
        )
      : [];
    return models.length ? [{ endpoint, models }] : [];
  });
  const current = isConfiguredCustomModel(selected, endpoints)
    ? getCompatModelInfo(selected, endpoints)
    : undefined;
  const restoreFocus = (event: Event) => {
    event.preventDefault();
    if (!window.matchMedia("(pointer: coarse)").matches)
      composer.textareaRef.current?.focus();
  };
  if (!groups.length)
    return (
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className={triggerClass}
        onClick={() => void openSettingsWindow("models")}
      >
        <HugeiconsIcon icon={Settings01Icon} size={13} />
        {tr("Manage models")}
      </Button>
    );
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={composer.isBusy}
          className={cn(triggerClass, "shrink")}
          aria-label={tr("Select a model")}
          title={current?.label ?? tr("Select a model")}
        >
          <span className="max-w-48 truncate">
            {current?.label ?? tr("Select a model")}
          </span>
          <HugeiconsIcon icon={ArrowDown01Icon} size={11} />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        side="top"
        align="end"
        collisionPadding={8}
        className="min-w-48 max-w-[calc(100vw-1rem)]"
        onCloseAutoFocus={restoreFocus}
      >
        {groups.map(({ endpoint, models }) => (
          <DropdownMenuSub key={endpoint.id}>
            <DropdownMenuSubTrigger className="gap-3">
              <span className="min-w-0 flex-1 truncate">
                {endpoint.name || tr("New provider")}
              </span>
              {models.some(
                (model) =>
                  endpointModelSelectionId(endpoint, model) === selected,
              ) && <HugeiconsIcon icon={Tick01Icon} size={13} />}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="max-h-72 min-w-48 max-w-[calc(100vw-1rem)] overflow-y-auto">
              <DropdownMenuRadioGroup
                value={selected}
                onValueChange={setSelected}
              >
                {models.map((model) => (
                  <DropdownMenuRadioItem
                    key={model.id}
                    value={endpointModelSelectionId(endpoint, model)}
                    className="gap-3"
                  >
                    <span className="min-w-0 flex-1 truncate" title={model.id}>
                      {model.id}
                    </span>
                    {effectiveCustomModel(model, endpoint.baseURL).vision && (
                      <span className="rounded bg-muted px-1 text-ui-xs text-muted-foreground">
                        {tr("Vision")}
                      </span>
                    )}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuSubContent>
          </DropdownMenuSub>
        ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={() => void openSettingsWindow("models")}>
          <HugeiconsIcon icon={Settings01Icon} size={14} />
          {tr("Manage models")}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

export function ComposerReasoningSelector() {
  const tr = useTranslation();
  useModelCatalogStore((s) => s.revision);
  const c = useComposer();
  const [openFor, setOpenFor] = useState<string | null>(null);
  const modelId = useChatStore((s) => s.selectedModelId);
  const sessionId = useChatStore((s) => s.activeSessionId);
  const selection = useChatStore(
    (s) =>
      (s.draftSession?.id === s.activeSessionId
        ? s.draftSession
        : s.sessions.find((session) => session.id === s.activeSessionId)
      )?.reasoningSelection,
  );
  const setLevel = useChatStore((s) => s.setSessionReasoningLevel);
  const effortColor = usePreferencesStore((s) => s.effortColor);
  const endpoints = usePreferencesStore((s) => s.customEndpoints);
  const resolved = resolveEndpointModel(modelId, endpoints);
  const levels = resolved
    ? effectiveCustomModel(resolved.model, resolved.endpoint.baseURL)
        .reasoningLevels
    : [];
  const context = JSON.stringify([modelId, sessionId, levels]);
  const open = openFor === context && !c.isBusy;
  useEffect(() => {
    if (openFor !== context || c.isBusy) setOpenFor(null);
  }, [context, openFor, c.isBusy]);
  if (!levels.length || !isConfiguredCustomModel(modelId, endpoints))
    return null;
  const level = selectedReasoningLevel(modelId, levels, selection);
  const label = level
    ? tr(reasoningLevelLabel(level))
    : tr("Select reasoning level");
  const index = level ? levels.indexOf(level) : -1;
  const content = (
    <>
      <HugeiconsIcon icon={BrainIcon} size={13} />
      <span>{label}</span>
    </>
  );
  if (levels.length === 1 && level)
    return (
      <span
        className={cn(triggerClass, "inline-flex shrink-0 items-center")}
        title={tr("Reasoning intensity")}
      >
        {content}
      </span>
    );
  return (
    <Popover
      open={open}
      onOpenChange={(next) => setOpenFor(next ? context : null)}
    >
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className={cn(triggerClass, "shrink-0")}
          disabled={c.isBusy || !sessionId}
          title={tr("Reasoning intensity")}
          aria-label={tr("Reasoning intensity")}
        >
          {content}
          <HugeiconsIcon icon={ArrowDown01Icon} size={11} />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        side="top"
        align="end"
        sideOffset={8}
        collisionPadding={8}
        className="reasoning-effort-panel w-80 max-w-[calc(100vw-1rem)] gap-4 rounded-xl p-4"
        data-effort-color={effortColor}
        aria-label={tr("Reasoning intensity")}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          if (!window.matchMedia("(pointer: coarse)").matches)
            c.textareaRef.current?.focus();
        }}
      >
        <div className="flex min-h-7 items-center justify-between gap-3">
          <span className="shrink-0 text-ui-caption text-muted-foreground">
            {tr("Reasoning intensity")}
          </span>
          <span
            className="reasoning-effort-panel__value truncate text-ui-lg font-medium"
            data-max={index === levels.length - 1 || undefined}
            title={label}
          >
            {label}
          </span>
        </div>
        <Suspense fallback={<div className="h-9" />}>
          <EffortSlider
            color={effortColor}
            count={levels.length}
            index={index}
            label={tr("Reasoning intensity")}
            valueText={label}
            active={open}
            disabled={c.isBusy || !sessionId}
            onIndexChange={(next) => {
              const value = levels[next];
              if (sessionId && value !== undefined)
                setLevel(sessionId, modelId, value);
            }}
          />
        </Suspense>
      </PopoverContent>
    </Popover>
  );
}

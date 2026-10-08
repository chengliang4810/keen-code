import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSub,
  DropdownMenuSubTrigger,
  DropdownMenuSubContent,
  DropdownMenuSeparator,
  DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  endpointModels,
  endpointModelSelectionId,
  effectiveCustomModel,
  resolveEndpointModel,
} from "@/modules/ai/config";
import { reasoningLevelLabel } from "@/modules/ai/lib/reasoning";
import { useModelCatalogStore } from "@/modules/ai/lib/modelCatalogState";
import { usePreferencesStore } from "@/modules/settings/preferences";
import { openSettingsWindow } from "@/modules/settings/openSettingsWindow";
import { useTranslation } from "@/modules/i18n";

export type SubagentModelSelection = {
  modelId?: string;
  reasoningLevel?: string;
};
export function SubagentModelFields({
  value,
  disabled,
  onChange,
}: {
  value: SubagentModelSelection;
  disabled?: boolean;
  onChange: (value: SubagentModelSelection) => void;
}) {
  const tr = useTranslation();
  useModelCatalogStore((s) => s.revision);
  const endpoints = usePreferencesStore((s) => s.customEndpoints);
  const resolved = value.modelId
    ? resolveEndpointModel(value.modelId, endpoints)
    : undefined;
  const levels = resolved
    ? effectiveCustomModel(resolved.model, resolved.endpoint.baseURL)
        .reasoningLevels
    : [];
  const groups = endpoints.flatMap((endpoint) => {
    const models = endpoint.baseURL.trim()
      ? endpointModels(endpoint).filter(
          (model) => model.enabled !== false && model.id.trim(),
        )
      : [];
    return models.length ? [{ endpoint, models }] : [];
  });
  const chooseModel = (id: string) => {
    if (id === "inherit")
      return onChange({ modelId: undefined, reasoningLevel: undefined });
    const model = resolveEndpointModel(id, endpoints);
    if (!model) return;
    const available = effectiveCustomModel(
      model.model,
      model.endpoint.baseURL,
    ).reasoningLevels;
    onChange({ modelId: id, reasoningLevel: available[available.length - 1] });
  };
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-2">
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={disabled}
            aria-label={tr("Select a model")}
            className="h-8 max-w-56 truncate font-normal"
          >
            {resolved
              ? `${resolved.endpoint.name} / ${resolved.model.id}`
              : value.modelId
                ? tr("Select a model")
                : tr("Inherit")}
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent
          side="bottom"
          align="start"
          className="max-h-80 min-w-48 overflow-y-auto"
        >
          <DropdownMenuRadioGroup
            value={value.modelId ?? "inherit"}
            onValueChange={chooseModel}
          >
            <DropdownMenuRadioItem value="inherit">
              {tr("Inherit")}
            </DropdownMenuRadioItem>
            {groups.map(({ endpoint, models }) => (
              <DropdownMenuSub key={endpoint.id}>
                <DropdownMenuSubTrigger>
                  {endpoint.name || tr("New provider")}
                </DropdownMenuSubTrigger>
                <DropdownMenuSubContent className="max-h-72 overflow-y-auto">
                  {models.map((model) => (
                    <DropdownMenuRadioItem
                      key={model.id}
                      value={endpointModelSelectionId(endpoint, model)}
                    >
                      {model.id}
                    </DropdownMenuRadioItem>
                  ))}
                </DropdownMenuSubContent>
              </DropdownMenuSub>
            ))}
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => void openSettingsWindow("models")}>
            {tr("Manage models")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {levels.length > 0 && (
        <Select
          value={value.reasoningLevel ?? levels[levels.length - 1]}
          disabled={disabled}
          onValueChange={(reasoningLevel) =>
            onChange({ modelId: value.modelId, reasoningLevel })
          }
        >
          <SelectTrigger
            size="sm"
            aria-label={tr("Reasoning intensity")}
            className="rounded-md border-border text-ui-base"
          >
            <SelectValue placeholder={tr("Select reasoning level")} />
          </SelectTrigger>
          <SelectContent position="popper">
            {levels.map((level) => (
              <SelectItem key={level} value={level}>
                {tr(reasoningLevelLabel(level))}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      )}
    </div>
  );
}

import { Button } from "@/components/ui/button";
/** Composer model menu. */

import { findModel, type ModelOption } from "@/lib/modelCatalog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuPortal,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tip } from "@/components/ui/tooltip";
import {
  IconCheck,
  IconChevronDown,
  IconPlus,
} from "@/components/icons";

/* ---------- Model ---------- */

export interface ComposerModelMenuProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 当前模型所属供应商。 */
  providerId?: string | null;
  modelId: string;
  /** Live selectable models only (from Host catalog). */
  models?: ModelOption[];
  labels: {
    model: string;
    addModel: string;
  };
  onModel: (id: string, providerId?: string) => void;
  /** 无供应商配置时打开模型设置。 */
  onAddModel: () => void;
}

/** 模型供应商及其可选模型，用于构建级联菜单。 */
export interface ComposerModelProviderGroup {
  /** 供应商稳定标识。 */
  id: string;
  /** 供应商显示名称。 */
  label: string;
  /** 当前供应商下可选择的模型。 */
  models: ModelOption[];
}

/** 按供应商归并模型，并保持 Host 模型目录的原始顺序。 */
export function groupComposerModelsByProvider(
  models: ModelOption[],
): ComposerModelProviderGroup[] {
  return Array.from(
    models.reduce((groups, model) => {
      const id = model.providerId?.trim();
      const label = model.providerLabel?.trim();
      if (!id || !label) {
        throw new Error(`模型 ${model.id} 缺少供应商信息`);
      }
      const group = groups.get(id) ?? {
        id,
        label,
        models: [] as ModelOption[],
      };
      group.models.push(model);
      groups.set(id, group);
      return groups;
    }, new Map<string, ComposerModelProviderGroup>()),
  ).map(([, group]) => group);
}

export function ComposerModelMenu({
  open,
  onOpenChange,
  providerId,
  modelId,
  models = [],
  labels,
  onModel,
  onAddModel,
}: ComposerModelMenuProps) {
  const modelList = models;
  const activeModel =
    modelList.find(
      (model) =>
        model.id === modelId &&
        (!providerId || model.providerId === providerId),
    ) ?? findModel(modelId, modelList);
  const providerGroups = groupComposerModelsByProvider(modelList);

  const modelLabel = activeModel?.label ?? modelId;
  const triggerText = modelLabel;
  const title = `${labels.model}: ${modelLabel}`;

  if (modelList.length === 0) {
    return (
      <div className="cmm cmm--model cmm--model-empty">
        <Button
          type="button"
          className="cmm__trigger"
          aria-label={labels.addModel}
          title={labels.addModel}
          onClick={onAddModel}
        >
          <span className="cmm__icon" aria-hidden>
            <IconPlus size={14} />
          </span>
          <span className="cmm__trigger-text cmm__trigger-text--full">
            {labels.addModel}
          </span>
        </Button>
      </div>
    );
  }

  const trigger = (
    <DropdownMenuTrigger asChild>
      <Button
        type="button"
        className="cmm__trigger"
        aria-label={labels.model}
      >
        <span className="cmm__trigger-text cmm__trigger-text--full">
          {triggerText}
        </span>
        <span className="cmm__trigger-text cmm__trigger-text--short">
          {modelLabel}
        </span>
        <span className="cmm__chev" aria-hidden>
          <IconChevronDown size={12} />
        </span>
      </Button>
    </DropdownMenuTrigger>
  );

  return (
    <DropdownMenu open={open} onOpenChange={onOpenChange}>
      <div className={`cmm cmm--model ${open ? "is-open" : ""}`}>
        <Tip label={title}>{trigger}</Tip>
      </div>
      <DropdownMenuContent
        className="cmm__dropdown-content w-56"
        align="start"
        sideOffset={8}
      >
        <DropdownMenuLabel>{labels.model}</DropdownMenuLabel>
        <DropdownMenuGroup>
          {providerGroups.map((provider) => (
            <DropdownMenuSub key={provider.id}>
              <DropdownMenuSubTrigger
                className={
                  provider.id === providerId ? "cmm__dropdown-active" : undefined
                }
              >
                <span className="truncate">{provider.label}</span>
              </DropdownMenuSubTrigger>
              <DropdownMenuPortal>
                <DropdownMenuSubContent className="cmm__dropdown-content cmm__model-list w-56">
                  <DropdownMenuGroup>
                    {provider.models.map((model) => {
                      const selected =
                        model.id === modelId &&
                        (!providerId || model.providerId === providerId);
                      return (
                        <DropdownMenuItem
                          key={`${provider.id}:${model.id}`}
                          className={selected ? "cmm__dropdown-active" : undefined}
                          onSelect={() => onModel(model.id, model.providerId)}
                        >
                          <span className="truncate">{model.label}</span>
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

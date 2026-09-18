import { Button } from "@/components/ui/button";
/** Composer model menu. */

import { findActiveModel, type ModelOption } from "@/lib/modelCatalog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuPortal,
  DropdownMenuSeparator,
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
  /** 当前会话实际绑定的供应商；为空时按模型 ID 兜底匹配。 */
  providerId?: string | null;
  modelId: string;
  /** Live selectable models only (from Host catalog). */
  models?: ModelOption[];
  labels: {
    model: string;
    addModel: string;
    manageModels: string;
    /** 模型图片输入能力标签。 */
    vision: string;
  };
  onModel: (id: string, providerId?: string) => void;
  /** 打开模型设置。 */
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
  // 同一模型 ID 可能属于多个供应商；必须按会话自身供应商解析，否则触发器会
  // 显示成全局活跃供应商，与运行时实际使用的路由不一致。
  const activeModel = findActiveModel(modelId, providerId, modelList);
  const providerGroups = groupComposerModelsByProvider(modelList);

  const activeProviderId = activeModel?.providerId ?? providerId ?? null;
  const modelLabel = activeModel?.label ?? modelId;
  const providerLabel = activeModel?.providerLabel?.trim();
  const triggerText = providerLabel
    ? `${providerLabel}/${modelLabel}`
    : modelLabel;
  const title = `${labels.model}: ${triggerText}`;

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
          {providerGroups.map((provider) => {
            const providerActive = provider.id === activeProviderId;
            return (
              <DropdownMenuSub key={provider.id}>
                <DropdownMenuSubTrigger
                  className={
                    providerActive ? "cmm__dropdown-active" : undefined
                  }
                >
                  <span className="min-w-0 flex-1 truncate">
                    {provider.label}
                  </span>
                  {providerActive ? (
                    <span aria-hidden>
                      <IconCheck size={16} />
                    </span>
                  ) : null}
                </DropdownMenuSubTrigger>
                <DropdownMenuPortal>
                  <DropdownMenuSubContent className="cmm__dropdown-content cmm__model-list w-56">
                    <DropdownMenuGroup>
                      {provider.models.map((model) => {
                        const selected =
                          model.id === modelId &&
                          model.providerId === activeProviderId;
                        return (
                          <DropdownMenuItem
                            key={`${provider.id}:${model.id}`}
                            className={
                              selected ? "cmm__dropdown-active" : undefined
                            }
                            onSelect={() => onModel(model.id, model.providerId)}
                          >
                            <span className="min-w-0 truncate">{model.label}</span>
                            {model.supportsVision ? (
                              <span className="cmm__badge">
                                {labels.vision}
                              </span>
                            ) : null}
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
            );
          })}
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={onAddModel}>
          <span className="truncate">{labels.manageModels}</span>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

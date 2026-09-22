import { Button } from "@/components/ui/button";
import { Badge } from "@appica/ui-react/badge";
/** Composer model menu. */

import { findActiveModel, type ModelOption } from "@/lib/modelCatalog";
import { IconCode, IconPlus } from "@/components/icons";
import { ProviderModelMenu } from "@/components/ProviderModelMenu";

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
  if (modelList.length === 0) {
    return (
      <div className="cmm cmm--model cmm--model-empty">
        <Button
          type="button"
          variant="ghost"
          size="md"
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

  return (
    <ProviderModelMenu
      open={open}
      onOpenChange={onOpenChange}
      groups={providerGroups.map((provider) => ({
        id: provider.id,
        label: provider.label,
        models: provider.models.map((model) => ({
          id: model.id,
          label: model.label,
          suffix: model.supportsVision ? (
            <Badge size="md" variant="soft">{labels.vision}</Badge>
          ) : undefined,
        })),
      }))}
      selectedProviderId={activeProviderId}
      selectedModelId={modelId}
      triggerLabel={labels.model}
      triggerTitle={`${labels.model}: ${triggerText}`}
      triggerVariant="ghost"
      triggerWrapperClassName={`cmm cmm--model ${open ? "is-open" : ""}`}
      triggerClassName="cmm__trigger"
      triggerContent={
        <>
          <span className="cmm__icon cmm__model-icon" aria-hidden>
            <IconCode size={14} />
          </span>
          {providerLabel ? (
            <span className="cmm__trigger-text cmm__trigger-text--provider">
              {providerLabel}/
            </span>
          ) : null}
          <span className="cmm__trigger-text cmm__trigger-text--full">
            {modelLabel}
          </span>
          <span className="cmm__trigger-text cmm__trigger-text--short">
            {modelLabel}
          </span>
        </>
      }
      contentClassName="cmm__dropdown-content w-56"
      modelContentClassName="cmm__dropdown-content cmm__model-list w-56"
      sideOffset={8}
      footerAction={{ label: labels.manageModels, onSelect: onAddModel }}
      onModelSelect={(nextProviderId, nextModelId) => onModel(nextModelId, nextProviderId)}
    />
  );
}

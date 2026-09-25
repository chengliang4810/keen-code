import { IconBrain, IconBolt, IconChevronDown } from "@/components/icons";
import { Button } from "@appica/ui-react/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { EffortSlider } from "@/components/ui/effort-slider";
import { Tip } from "@/components/ui/tooltip";
import {
  effortDisplayLabel,
  effortsForModel,
  type ModelOption,
} from "@/lib/modelCatalog";
import { resolveEffortTrackKind } from "@/lib/effortTrack";
import "@/styles/effort-slider.css";

export interface ComposerReasoningMenuProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  model?: ModelOption;
  effort: string;
  ultra: boolean;
  labels: {
    reasoning: string;
    reasoningUnsupported: string;
    ultra: string;
    effortNone: string;
    effortMinimal: string;
    effortHigh: string;
    effortMedium: string;
    effortLow: string;
    effortXHigh: string;
    effortMax: string;
  };
  onEffort: (id: string) => void;
  onUltra: (enabled: boolean) => void;
}

function effortLabel(
  id: string,
  model: ModelOption | undefined,
  labels: ComposerReasoningMenuProps["labels"],
): string {
  const entry = effortsForModel(model).find((effort) => effort.id === id);
  return effortDisplayLabel(entry ?? id, {
    none: labels.effortNone,
    minimal: labels.effortMinimal,
    high: labels.effortHigh,
    medium: labels.effortMedium,
    low: labels.effortLow,
    xhigh: labels.effortXHigh,
    max: labels.effortMax,
  });
}

/** 标题样式类：与滑块的四态同源，最高档、快速与融合各有颜色。 */
export function effortTitleClass(
  isMax: boolean,
  fast: boolean,
): string {
  const kind = resolveEffortTrackKind(isMax, fast);
  return `effort-title effort-title--${kind}`;
}

/** 模型推理强度与 Ultra 委派策略的独立面板。 */
export function ComposerReasoningMenu({
  open,
  onOpenChange,
  model,
  effort,
  ultra,
  labels,
  onEffort,
  onUltra,
}: ComposerReasoningMenuProps) {
  if (!model) return null;

  const effortList = effortsForModel(model);
  const effortIndex = effortList.findIndex((entry) => entry.id === effort);
  const hasEffort = model?.reasoningSupported === true && effortList.length > 0;
  const currentLabel = hasEffort
    ? effortLabel(effortList[Math.max(0, effortIndex)]!.id, model, labels)
    : labels.reasoningUnsupported;
  // 标题与滑块共用四态：最高档由档位决定，快速复用 Ultra 开关。
  const isMax = hasEffort && effortList.length > 1 && effortIndex === effortList.length - 1;

  const trigger = (
      <Tip label={`${labels.reasoning}: ${currentLabel}`}>
      <DropdownMenuTrigger render={<Button
        type="button"
        variant="ghost"
        size="md"
        className="cmm__trigger"
        aria-label={`${labels.reasoning}: ${currentLabel}`}
      />}>
        <span className="cmm__icon" aria-hidden>
          <IconBrain size={14} />
        </span>
        <span className="cmm__trigger-text cmm__trigger-text--full">
          {currentLabel}
        </span>
        <span className="cmm__chev" aria-hidden>
          <IconChevronDown size={12} />
        </span>
    </DropdownMenuTrigger>
    </Tip>
  );

  return (
    <DropdownMenu open={open} onOpenChange={onOpenChange}>
      <div className={`cmm cmm--reasoning ${open ? "is-open" : ""}`}>
        {trigger}
      </div>
      <DropdownMenuContent
        className="cmm__dropdown-content w-80 p-4"
        align="end"
        sideOffset={8}
      >
        <div className="grid gap-4">
          {/* 上游 EffortSliderCard 布局：左上名称、中央大标题、右上快速按钮。 */}
          <div className="effort-panel__head">
            <label
              htmlFor="composer-reasoning-effort"
              className="effort-panel__model"
            >
              {labels.reasoning}
            </label>
            <span className={effortTitleClass(isMax, ultra)}>{currentLabel}</span>
            <Tip label={labels.ultra}>
              <Button
                type="button"
                className="effort-panel__fast"
                variant={ultra ? "primary" : "ghost"}
                size="icon-md"
                aria-pressed={ultra}
                aria-label={labels.ultra}
                onClick={() => onUltra(!ultra)}
              >
                <IconBolt size={15} />
              </Button>
            </Tip>
          </div>
          <EffortSlider
            id="composer-reasoning-effort"
            count={hasEffort ? effortList.length : 0}
            index={Math.max(0, effortIndex)}
            onIndexChange={(next) => {
              const entry = effortList[next];
              if (entry) onEffort(entry.id);
            }}
            fast={ultra}
            label={labels.reasoning}
          />
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

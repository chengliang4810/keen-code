import { IconBrain, IconChevronDown } from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { Tip } from "@/components/ui/tooltip";
import {
  effortDisplayLabel,
  effortsForModel,
  type ModelOption,
} from "@/lib/modelCatalog";

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
    ultraDescription: string;
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
  const effortList = effortsForModel(model);
  const effortIndex = effortList.findIndex((entry) => entry.id === effort);
  const hasEffort = model?.reasoningSupported === true && effortList.length > 0;
  const currentLabel = hasEffort
    ? effortLabel(effortList[Math.max(0, effortIndex)]!.id, model, labels)
    : labels.reasoningUnsupported;

  const trigger = (
    <DropdownMenuTrigger asChild>
      <Button
        type="button"
        className="cmm__trigger"
        aria-label={`${labels.reasoning}: ${currentLabel}`}
      >
        <span className="cmm__icon" aria-hidden>
          <IconBrain size={14} />
        </span>
        <span className="cmm__trigger-text cmm__trigger-text--full">
          {currentLabel}
        </span>
        <span className="cmm__chev" aria-hidden>
          <IconChevronDown size={12} />
        </span>
      </Button>
    </DropdownMenuTrigger>
  );

  return (
    <DropdownMenu open={open} onOpenChange={onOpenChange}>
      <div className={`cmm cmm--reasoning ${open ? "is-open" : ""}`}>
        <Tip label={`${labels.reasoning}: ${currentLabel}`}>{trigger}</Tip>
      </div>
      <DropdownMenuContent
        className="cmm__dropdown-content w-80 p-4"
        align="end"
        sideOffset={8}
      >
        <div className="grid gap-4">
          <div className="grid gap-3">
            <div className="flex items-center justify-between gap-2">
              <Label htmlFor="composer-reasoning-effort">{labels.reasoning}</Label>
              <span className="text-sm text-muted-foreground">{currentLabel}</span>
            </div>
            <Slider
              id="composer-reasoning-effort"
              aria-label={labels.reasoning}
              value={[Math.max(0, effortIndex)]}
              onValueChange={([index]) => {
                const next = effortList[index ?? -1];
                if (next) onEffort(next.id);
              }}
              min={0}
              max={Math.max(0, effortList.length - 1)}
              step={1}
              disabled={!hasEffort || effortList.length < 2}
            />
          </div>
          <DropdownMenuSeparator />
          <div className="flex items-start justify-between gap-4">
            <div className="grid gap-1">
              <Label htmlFor="composer-ultra-mode">{labels.ultra}</Label>
              <span className="text-xs text-muted-foreground">
                {labels.ultraDescription}
              </span>
            </div>
            <Switch
              id="composer-ultra-mode"
              aria-label={labels.ultra}
              checked={ultra}
              onCheckedChange={onUltra}
            />
          </div>
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

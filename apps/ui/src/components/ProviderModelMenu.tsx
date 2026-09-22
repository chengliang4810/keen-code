import type { ReactNode } from "react";
import { Button, type ButtonProps } from "@/components/ui/button";
import { IconCheck, IconChevronDown } from "@/components/icons";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

export interface ProviderModelMenuItem {
  id: string;
  label: string;
  suffix?: ReactNode;
}

export interface ProviderModelMenuGroup {
  id: string;
  label: string;
  models: ProviderModelMenuItem[];
}

interface ProviderModelMenuProps {
  groups: ReadonlyArray<ProviderModelMenuGroup>;
  selectedProviderId?: string | null;
  selectedModelId?: string | null;
  triggerContent: ReactNode;
  triggerLabel: string;
  triggerTitle?: string;
  triggerId?: string;
  triggerClassName?: string;
  triggerWrapperClassName?: string;
  triggerVariant?: ButtonProps["variant"];
  /** Composer 触发器统一使用 Appica 的 md 控件几何，避免菜单触发器高度漂移。 */
  triggerSize?: ButtonProps["size"];
  disabled?: boolean;
  align?: "start" | "center" | "end";
  sideOffset?: number;
  contentClassName?: string;
  modelContentClassName?: string;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  emptyOption?: {
    label: string;
    selected: boolean;
    onSelect: () => void;
  };
  footerAction?: {
    label: string;
    onSelect: () => void;
  };
  onModelSelect: (providerId: string, modelId: string) => void;
}

/** 供应商一级、模型二级的统一模型选择菜单。 */
export function ProviderModelMenu({
  groups,
  selectedProviderId,
  selectedModelId,
  triggerContent,
  triggerLabel,
  triggerTitle,
  triggerId,
  triggerClassName,
  triggerWrapperClassName,
  triggerVariant = "outline",
  triggerSize = "md",
  disabled,
  align = "start",
  sideOffset = 6,
  contentClassName,
  modelContentClassName,
  open,
  onOpenChange,
  emptyOption,
  footerAction,
  onModelSelect,
}: ProviderModelMenuProps) {
  const trigger = (
    <DropdownMenuTrigger
      render={
        <Button
          id={triggerId}
          type="button"
          variant={triggerVariant}
          size={triggerSize}
          className={triggerClassName}
          disabled={disabled}
          aria-label={triggerLabel}
          title={triggerTitle}
        />
      }
    >
      {triggerContent}
      <IconChevronDown size={12} className="chevron" aria-hidden />
    </DropdownMenuTrigger>
  );

  return (
    <DropdownMenu size="md" open={open} onOpenChange={onOpenChange}>
      {triggerWrapperClassName ? (
        <div className={triggerWrapperClassName}>{trigger}</div>
      ) : trigger}
      <DropdownMenuContent
        align={align}
        sideOffset={sideOffset}
        className={contentClassName}
      >
        {emptyOption ? (
          <>
            <DropdownMenuGroup>
              <DropdownMenuItem onClick={emptyOption.onSelect}>
                <span className="min-w-0 flex-1 truncate">{emptyOption.label}</span>
                {emptyOption.selected ? <IconCheck size={16} aria-hidden /> : null}
              </DropdownMenuItem>
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
          </>
        ) : null}
        <DropdownMenuGroup>
          {groups.map((group) => {
            const providerSelected = group.id === selectedProviderId;
            return (
              <DropdownMenuSub key={group.id}>
                <DropdownMenuSubTrigger>
                  <span className="min-w-0 flex-1 truncate">{group.label}</span>
                  {providerSelected ? <IconCheck size={16} aria-hidden /> : null}
                </DropdownMenuSubTrigger>
                <DropdownMenuSubContent className={modelContentClassName}>
                  <DropdownMenuGroup>
                    {group.models.map((model) => {
                      const selected = providerSelected && model.id === selectedModelId;
                      return (
                        <DropdownMenuItem
                          key={model.id}
                          onClick={() => onModelSelect(group.id, model.id)}
                        >
                          <span className="min-w-0 flex-1 truncate">{model.label}</span>
                          {model.suffix}
                          {selected ? <IconCheck size={16} aria-hidden /> : null}
                        </DropdownMenuItem>
                      );
                    })}
                  </DropdownMenuGroup>
                </DropdownMenuSubContent>
              </DropdownMenuSub>
            );
          })}
        </DropdownMenuGroup>
        {footerAction ? (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={footerAction.onSelect}>
              <span className="truncate">{footerAction.label}</span>
            </DropdownMenuItem>
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

import type { ReactElement, ReactNode } from "react";
import {
  Tooltip as AppicaTooltip,
  TooltipContent as AppicaTooltipContent,
  TooltipProvider as AppicaTooltipProvider,
  TooltipTrigger as AppicaTooltipTrigger,
  type TooltipContentProps as AppicaTooltipContentProps,
  type TooltipProviderProps as AppicaTooltipProviderProps,
  type TooltipProps as AppicaTooltipProps,
  type TooltipTriggerProps as AppicaTooltipTriggerProps,
} from "@appica/ui-react/tooltip";

import { cn } from "@/lib/utils";

export function TooltipProvider({ delay = 0, ...props }: AppicaTooltipProviderProps) {
  return <AppicaTooltipProvider delay={delay} {...props} />;
}

export function Tooltip(props: AppicaTooltipProps) {
  return <AppicaTooltip {...props} />;
}

export function TooltipTrigger(props: AppicaTooltipTriggerProps) {
  return <AppicaTooltipTrigger {...props} />;
}

export function TooltipContent({ className, ...props }: AppicaTooltipContentProps) {
  return (
    <AppicaTooltipContent
      className={cn(
        "rounded-lg border border-border bg-tooltip px-3 py-1.5 text-xs text-tooltip-foreground shadow-md",
        className,
      )}
      {...props}
    />
  );
}

export type TipPlacement = "top" | "bottom";

/** KeenCode 的紧凑提示语义，定位、碰撞处理与无障碍行为全部由 Appica UI 提供。 */
export function Tip({
  label,
  children,
  placement = "top",
  delayMs = 0,
  disabled = false,
  className,
}: {
  label: ReactNode;
  children: ReactElement;
  placement?: TipPlacement;
  delayMs?: number;
  disabled?: boolean;
  className?: string;
}) {
  if (disabled || label == null || label === "") return children;

  return (
    <TooltipProvider delay={delayMs}>
      <Tooltip>
        <TooltipTrigger render={children} />
        <TooltipContent side={placement} arrow={false} className={className}>
          {label}
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}

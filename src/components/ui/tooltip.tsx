import type { ReactElement, ReactNode } from "react";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@appica/ui-react/tooltip";

export type TipPlacement = "top" | "bottom";

/** KeenCode 的紧凑提示语义，定位、碰撞处理与无障碍行为全部由 Appica UI 提供。 */
export function Tip({
  label,
  children,
  placement = "top",
  delayMs = 420,
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

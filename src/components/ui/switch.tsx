import {
  Switch as AppicaSwitch,
  type SwitchProps as AppicaSwitchProps,
} from "@appica/ui-react/switch";

import { cn } from "@/lib/utils";

export type SwitchSize = "sm" | "default" | "md";
export type SwitchProps = Omit<AppicaSwitchProps, "size"> & { size?: SwitchSize };

/** 保留 ZCode 的本地几何别名，但 Appica 状态原语始终使用 md。 */
export function Switch({ className, size = "md", ...props }: SwitchProps) {
  const effectiveSize = size === "default" ? "md" : size;
  return (
    <AppicaSwitch
      size="md"
      data-slot="switch"
      data-size={effectiveSize}
      className={cn(
        "keencode-switch peer group/switch relative border border-transparent p-px outline-none after:absolute after:-inset-x-3 after:-inset-y-2 focus-visible:ring-2 focus-visible:ring-input-border-focused/30 data-checked:bg-primary data-unchecked:bg-primary/30",
        effectiveSize === "sm" ? "h-4 w-7" : "h-[18px] w-8",
        className,
      )}
      {...props}
    />
  );
}

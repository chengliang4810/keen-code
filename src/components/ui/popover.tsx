import * as React from "react";
import * as PopoverPrimitive from "@radix-ui/react-popover";
import { cn } from "@/lib/utils";

// shadcn Popover 的 Portal、焦点恢复、Esc 和碰撞定位沿用 Radix 原语。
function Popover(props: React.ComponentProps<typeof PopoverPrimitive.Root>) {
  return <PopoverPrimitive.Root {...props} />;
}
function PopoverTrigger(props: React.ComponentProps<typeof PopoverPrimitive.Trigger>) {
  return <PopoverPrimitive.Trigger data-slot="popover-trigger" {...props} />;
}
function PopoverContent({ className, sideOffset = 8, align = "start", ...props }:
  React.ComponentProps<typeof PopoverPrimitive.Content>) {
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Content
        data-slot="popover-content"
        className={cn("ui-stat-popover", className)}
        align={align}
        sideOffset={sideOffset}
        collisionPadding={12}
        {...props}
      />
    </PopoverPrimitive.Portal>
  );
}
export { Popover, PopoverTrigger, PopoverContent };

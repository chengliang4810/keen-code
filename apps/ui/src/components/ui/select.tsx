import * as React from "react";
import {
  Select as AppicaSelect,
  SelectContent as AppicaSelectContent,
  SelectGroup as AppicaSelectGroup,
  SelectGroupLabel as AppicaSelectGroupLabel,
  SelectItem as AppicaSelectItem,
  SelectSeparator as AppicaSelectSeparator,
  SelectTrigger as AppicaSelectTrigger,
  SelectValue as AppicaSelectValue,
  type SelectContentProps as AppicaSelectContentProps,
  type SelectItemProps as AppicaSelectItemProps,
  type SelectProps as AppicaSelectProps,
  type SelectTriggerProps as AppicaSelectTriggerProps,
} from "@appica/ui-react/select";

import { cn } from "@/lib/utils";

/**
 * 共享 Select 入口。Appica 负责尺寸（size="md" 统一缩放触发器高度、弹层圆角、
 * 条目内边距与图标尺寸）、variant、多选与无障碍行为；这里只保留项目菜单表面
 * 色（弹层 bg-popover / border-border）与布局守卫，不再覆盖官方几何。
 */
export function Select(props: AppicaSelectProps) {
  return <AppicaSelect size="md" {...props} />;
}

export function SelectGroup({ className, ...props }: React.ComponentProps<typeof AppicaSelectGroup>) {
  return <AppicaSelectGroup className={cn(className)} {...props} />;
}

export function SelectValue({ className, ...props }: React.ComponentProps<typeof AppicaSelectValue>) {
  return <AppicaSelectValue className={cn(className)} {...props} />;
}

export function SelectTrigger({ className, ...props }: AppicaSelectTriggerProps) {
  return <AppicaSelectTrigger className={cn(className)} {...props} />;
}

export interface SelectContentProps extends Omit<AppicaSelectContentProps, "className"> {
  position?: "item-aligned" | "popper";
  align?: "start" | "center" | "end";
  sideOffset?: number;
  className?: string;
}

export function SelectContent({
  className,
  position,
  align,
  sideOffset,
  ...props
}: SelectContentProps) {
  return (
    <AppicaSelectContent
      alignItemWithTrigger={position === undefined ? undefined : position !== "popper"}
      align={align}
      sideOffset={sideOffset}
      className={cn(
        "relative z-[60] border-border bg-popover text-foreground shadow-md [app-region:no-drag]",
        className,
      )}
      {...props}
    />
  );
}

export function SelectLabel({ className, ...props }: React.ComponentProps<typeof AppicaSelectGroupLabel>) {
  return <AppicaSelectGroupLabel className={cn("text-ui-base text-foreground-subtlest", className)} {...props} />;
}

export const SelectGroupLabel = SelectLabel;

export function SelectItem({ className, ...props }: AppicaSelectItemProps) {
  return (
    <AppicaSelectItem
      className={cn(
        "text-ui-base/relaxed text-foreground data-highlighted:bg-hover data-highlighted:text-foreground data-disabled:pointer-events-none data-disabled:text-foreground-subtlest data-disabled:opacity-100",
        className,
      )}
      {...props}
    />
  );
}

export function SelectSeparator({ className, ...props }: React.ComponentProps<typeof AppicaSelectSeparator>) {
  return <AppicaSelectSeparator className={cn("pointer-events-none", className)} {...props} />;
}

import {
  DropdownMenu as AppicaDropdownMenu,
  DropdownMenuCheckboxItem as AppicaDropdownMenuCheckboxItem,
  DropdownMenuContent as AppicaDropdownMenuContent,
  DropdownMenuGroup as AppicaDropdownMenuGroup,
  DropdownMenuGroupLabel as AppicaDropdownMenuGroupLabel,
  DropdownMenuItem as AppicaDropdownMenuItem,
  DropdownMenuLinkItem as AppicaDropdownMenuLinkItem,
  DropdownMenuRadioGroup as AppicaDropdownMenuRadioGroup,
  DropdownMenuRadioItem as AppicaDropdownMenuRadioItem,
  DropdownMenuSeparator as AppicaDropdownMenuSeparator,
  DropdownMenuSub as AppicaDropdownMenuSub,
  DropdownMenuSubContent as AppicaDropdownMenuSubContent,
  DropdownMenuSubTrigger as AppicaDropdownMenuSubTrigger,
  DropdownMenuTrigger as AppicaDropdownMenuTrigger,
  type DropdownMenuCheckboxItemProps,
  type DropdownMenuContentProps,
  type DropdownMenuGroupLabelProps,
  type DropdownMenuGroupProps,
  type DropdownMenuItemProps,
  type DropdownMenuLinkItemProps,
  type DropdownMenuProps as AppicaDropdownMenuProps,
  type DropdownMenuRadioGroupProps,
  type DropdownMenuRadioItemProps,
  type DropdownMenuSeparatorProps,
  type DropdownMenuSubContentProps,
  type DropdownMenuSubProps,
  type DropdownMenuSubTriggerProps,
  type DropdownMenuTriggerProps,
} from "@appica/ui-react/dropdown-menu";

import { cn } from "@/lib/utils";

export type DropdownMenuProps = Omit<AppicaDropdownMenuProps, "size"> & {
  /** Appica 下拉菜单统一使用中号；调用方不能覆盖底层尺寸。 */
  size?: "md";
};

/**
 * 共享下拉菜单入口。Appica 的 size（md）负责弹层圆角、条目内边距、图标尺寸
 * 与碰撞处理（max-h 由官方提供）；这里只保留项目菜单表面色、层级与 Tauri
 * 拖拽守卫。
 */
export function DropdownMenu({ size: _size, ...props }: DropdownMenuProps) {
  return <AppicaDropdownMenu {...props} size="md" />;
}

export function DropdownMenuTrigger({ className, ...props }: DropdownMenuTriggerProps) {
  return <AppicaDropdownMenuTrigger className={cn(className)} {...props} />;
}

const popupSurfaceClass =
  "z-[60] max-w-(--available-width) min-w-32 border-border bg-popover text-foreground shadow-md [app-region:no-drag]";

export function DropdownMenuContent({ className, ...props }: DropdownMenuContentProps) {
  return (
    <AppicaDropdownMenuContent
      className={cn(popupSurfaceClass, className)}
      {...props}
    />
  );
}

export function DropdownMenuGroup(props: DropdownMenuGroupProps) {
  return <AppicaDropdownMenuGroup {...props} />;
}

export function DropdownMenuGroupLabel({ className, ...props }: DropdownMenuGroupLabelProps) {
  return <AppicaDropdownMenuGroupLabel className={cn("text-ui-base text-foreground-subtlest", className)} {...props} />;
}

const itemClass =
  "text-ui-base/relaxed text-foreground data-highlighted:bg-hover data-highlighted:text-foreground data-disabled:pointer-events-none data-disabled:text-foreground-subtlest data-disabled:opacity-100";

export function DropdownMenuItem({ className, ...props }: DropdownMenuItemProps) {
  return <AppicaDropdownMenuItem className={cn(itemClass, className)} {...props} />;
}

export function DropdownMenuLinkItem({ className, ...props }: DropdownMenuLinkItemProps) {
  return <AppicaDropdownMenuLinkItem className={cn(itemClass, className)} {...props} />;
}

export function DropdownMenuRadioGroup(props: DropdownMenuRadioGroupProps) {
  return <AppicaDropdownMenuRadioGroup {...props} />;
}

export function DropdownMenuRadioItem({ className, ...props }: DropdownMenuRadioItemProps) {
  return <AppicaDropdownMenuRadioItem className={cn(itemClass, "justify-between", className)} {...props} />;
}

export function DropdownMenuCheckboxItem({ className, ...props }: DropdownMenuCheckboxItemProps) {
  return <AppicaDropdownMenuCheckboxItem className={cn(itemClass, "justify-between", className)} {...props} />;
}

export function DropdownMenuSeparator({ className, ...props }: DropdownMenuSeparatorProps) {
  return <AppicaDropdownMenuSeparator className={cn("pointer-events-none", className)} {...props} />;
}

export function DropdownMenuSub(props: DropdownMenuSubProps) {
  return <AppicaDropdownMenuSub {...props} />;
}

export function DropdownMenuSubTrigger({ className, ...props }: DropdownMenuSubTriggerProps) {
  return <AppicaDropdownMenuSubTrigger className={cn(itemClass, className)} {...props} />;
}

export function DropdownMenuSubContent({ className, ...props }: DropdownMenuSubContentProps) {
  return (
    <AppicaDropdownMenuSubContent
      className={cn(popupSurfaceClass, className)}
      {...props}
    />
  );
}

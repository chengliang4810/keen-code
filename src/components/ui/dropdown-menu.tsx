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

export function DropdownMenu({ size: _size, ...props }: DropdownMenuProps) {
  return <AppicaDropdownMenu {...props} size="md" />;
}

export function DropdownMenuTrigger({ className, ...props }: DropdownMenuTriggerProps) {
  return <AppicaDropdownMenuTrigger className={cn(className)} {...props} />;
}

export function DropdownMenuContent({ className, ...props }: DropdownMenuContentProps) {
  return (
    <AppicaDropdownMenuContent
      className={cn(
        "z-[60] flex max-h-(--available-height) max-w-(--available-width) min-w-32 flex-col gap-0.5 overflow-x-hidden overflow-y-auto rounded-lg border border-border bg-popover p-1 text-foreground shadow-md *:p-1",
        className,
      )}
      {...props}
    />
  );
}

export function DropdownMenuGroup(props: DropdownMenuGroupProps) {
  return <AppicaDropdownMenuGroup className="flex flex-col gap-0.5" {...props} />;
}

export function DropdownMenuGroupLabel({ className, ...props }: DropdownMenuGroupLabelProps) {
  return <AppicaDropdownMenuGroupLabel className={cn("px-2 pt-2 pb-1 text-ui-base text-foreground-subtlest", className)} {...props} />;
}

const itemClass = "group/dropdown-menu-item relative flex min-h-7 w-full cursor-default items-center gap-2 rounded-md px-2 py-1 text-ui-base/relaxed text-foreground outline-hidden select-none data-highlighted:bg-hover data-highlighted:text-foreground data-disabled:pointer-events-none data-disabled:text-foreground-subtlest data-disabled:opacity-100 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4";

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
  return <AppicaDropdownMenuSeparator className={cn("my-1 h-px bg-border", className)} {...props} />;
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
      className={cn(
        "z-[60] flex max-h-(--available-height) max-w-(--available-width) min-w-32 flex-col gap-0.5 overflow-x-hidden overflow-y-auto rounded-lg border border-border bg-popover p-1 text-foreground shadow-md *:p-1",
        className,
      )}
      {...props}
    />
  );
}

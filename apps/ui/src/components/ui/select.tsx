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

export type SelectSize = "xs" | "sm" | "default" | "md" | "lg";
export type SelectVariant =
  | "input"
  | "default"
  | "outline"
  | "secondary"
  | "ghost"
  | "destructive";

export interface SelectProps extends Omit<AppicaSelectProps, "size" | "variant"> {
  size?: SelectSize;
  variant?: SelectVariant;
}

function appicaSelectVariant(variant: SelectVariant): "outline" | "soft" {
  return variant === "input" || variant === "outline" ? "outline" : "soft";
}

export function Select({ size: _size = "md", variant = "input", ...props }: SelectProps) {
  return (
    <AppicaSelect
      size="md"
      variant={appicaSelectVariant(variant)}
      {...props}
    />
  );
}

export function SelectGroup({ className, ...props }: React.ComponentProps<typeof AppicaSelectGroup>) {
  return <AppicaSelectGroup className={cn("flex flex-col gap-0.5 scroll-my-1 p-1", className)} {...props} />;
}

export function SelectValue({ className, ...props }: React.ComponentProps<typeof AppicaSelectValue>) {
  return <AppicaSelectValue className={cn("min-w-0 flex-1 truncate text-start", className)} {...props} />;
}

export interface SelectTriggerProps
  extends Omit<AppicaSelectTriggerProps, "size" | "variant"> {
  variant?: SelectVariant;
  size?: SelectSize;
  indicator?: React.ReactNode;
}

export function SelectTrigger({
  className,
  variant = "input",
  size = "md",
  indicator,
  ...props
}: SelectTriggerProps) {
  const sizeClass = size === "xs" ? "h-5 rounded-sm pl-2 pr-1 text-ui-base [&_svg:not([class*='size-'])]:size-2.5" : size === "sm" ? "h-6 rounded-md pl-2 pr-1 text-ui-base/relaxed [&_svg:not([class*='size-'])]:size-3" : size === "lg" ? "h-8 rounded-lg pl-3 pr-2 text-ui-base [&_svg:not([class*='size-'])]:size-4" : "keencode-control-md h-7 rounded-md pl-2 pr-1 text-ui-base/relaxed [&_svg:not([class*='size-'])]:size-3.5";
  return (
    <AppicaSelectTrigger
      endSlot={indicator}
      className={cn(
        "flex w-fit items-center justify-between gap-1.5 border whitespace-nowrap transition-colors outline-none disabled:cursor-not-allowed disabled:opacity-50 data-placeholder:text-foreground-subtlest [&_svg]:pointer-events-none [&_svg]:shrink-0",
        (variant === "input" || variant === "outline") && "border-input-border bg-input hover:border-input-border-hover focus-visible:border-input-border-focused focus-visible:bg-input-focused focus-visible:ring-0 aria-invalid:border-destructive aria-invalid:ring-2 aria-invalid:ring-destructive/20 dark:aria-invalid:border-destructive/50 dark:aria-invalid:ring-destructive/40",
        variant === "default" && "border-transparent bg-primary text-primary-foreground hover:bg-primary/80",
        variant === "secondary" && "border-transparent bg-secondary text-secondary-foreground hover:bg-secondary/80",
        variant === "ghost" && "border-transparent bg-transparent hover:bg-muted",
        variant === "destructive" && "border-transparent bg-destructive text-destructive-foreground hover:bg-destructive/90",
        sizeClass,
        className,
      )}
      {...props}
    />
  );
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
        "relative z-[60] min-w-32 rounded-lg border border-border bg-popover p-1 text-foreground shadow-md [app-region:no-drag]",
        className,
      )}
      {...props}
    />
  );
}

export function SelectLabel({ className, ...props }: React.ComponentProps<typeof AppicaSelectGroupLabel>) {
  return <AppicaSelectGroupLabel className={cn("px-2 py-1.5 text-ui-base text-foreground-subtlest", className)} {...props} />;
}

export const SelectGroupLabel = SelectLabel;

export type SelectItemProps = AppicaSelectItemProps & { trailing?: React.ReactNode };
export function SelectItem({ className, trailing: _trailing, ...props }: SelectItemProps) {
  return <AppicaSelectItem className={cn("relative flex min-h-7 w-full cursor-default items-center gap-2 rounded-md px-2 py-1 text-ui-base/relaxed text-foreground outline-hidden select-none data-highlighted:bg-hover data-highlighted:text-foreground data-disabled:pointer-events-none data-disabled:text-foreground-subtlest data-disabled:opacity-100 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4", className)} {...props} />;
}

export function SelectSeparator({ className, ...props }: React.ComponentProps<typeof AppicaSelectSeparator>) {
  return <AppicaSelectSeparator className={cn("pointer-events-none my-1 h-px bg-border", className)} {...props} />;
}

export const SelectScrollUpButton = () => null;
export const SelectScrollDownButton = () => null;
export const selectTriggerVariants = {} as const;

import type { ButtonHTMLAttributes, Ref } from "react";
import { cn } from "@/lib/utils";

export type ButtonVariant =
  | "default"
  | "primary"
  | "primary-outline"
  | "secondary"
  | "soft"
  | "outline"
  | "ghost"
  | "destructive"
  | "warning"
  | "link"
  | "light";

export type ButtonSize =
  | "default"
  | "xs"
  | "sm"
  | "md"
  | "lg"
  | "icon"
  | "icon-xs"
  | "icon-sm"
  | "icon-md"
  | "icon-lg";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  ref?: Ref<HTMLButtonElement>;
}

const baseClass =
  "group/button inline-flex shrink-0 items-center justify-center border border-transparent bg-clip-padding text-ui-base/relaxed whitespace-nowrap outline-none select-none transition-colors disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-2 aria-invalid:ring-destructive/20 dark:aria-invalid:border-destructive/50 dark:aria-invalid:ring-destructive/40 [&_svg]:pointer-events-none [&_svg]:shrink-0";

const variantClass: Record<ButtonVariant, string> = {
  default: "bg-primary text-primary-foreground hover:bg-primary/80",
  primary: "bg-primary text-primary-foreground hover:bg-primary/80",
  "primary-outline":
    "border-primary bg-transparent text-primary hover:bg-primary hover:text-primary-foreground",
  secondary:
    "bg-secondary text-secondary-foreground hover:bg-secondary/80 aria-expanded:bg-secondary",
  soft: "bg-muted text-foreground hover:bg-hover",
  outline:
    "border-border bg-transparent text-foreground hover:border-border-strong hover:bg-input/50",
  ghost:
    "bg-transparent text-foreground hover:bg-hover aria-expanded:bg-hover",
  destructive:
    "bg-destructive text-destructive-foreground hover:bg-destructive/90 focus-visible:ring-destructive/20",
  warning:
    "bg-warning text-warning-foreground hover:bg-warning/90 focus-visible:ring-warning/20",
  link: "bg-transparent text-primary underline-offset-4 hover:underline",
  light: "border-white/10 bg-white/10 text-white hover:bg-white/15",
};

const sizeClass: Record<ButtonSize, string> = {
  default:
    "keencode-control-md h-7 gap-1 rounded-md px-2 text-ui-base [&_svg:not([class*='size-'])]:size-3.5",
  xs: "h-5 gap-1 rounded-sm px-2 text-ui-base [&_svg:not([class*='size-'])]:size-2.5",
  sm: "h-6 gap-1 rounded-md px-2 text-ui-base/relaxed [&_svg:not([class*='size-'])]:size-3",
  md: "keencode-control-md h-7 gap-1 rounded-md px-2 text-ui-base [&_svg:not([class*='size-'])]:size-3.5",
  lg: "h-8 gap-1 rounded-lg px-2.5 text-ui-base [&_svg:not([class*='size-'])]:size-4",
  icon: "size-7 rounded-md [&_svg:not([class*='size-'])]:size-4",
  "icon-xs": "size-5 rounded-sm [&_svg:not([class*='size-'])]:size-2.5",
  "icon-sm": "size-6 rounded-md [&_svg:not([class*='size-'])]:size-3",
  "icon-md": "size-7 rounded-lg [&_svg:not([class*='size-'])]:size-4",
  "icon-lg": "size-8 rounded-lg [&_svg:not([class*='size-'])]:size-4",
};

/**
 * ZCode 的共享按钮几何：默认 28px，图标按钮同为 28px。
 * KeenCode 保留既有命名，并提供 ZCode 的 default/warning/link 与紧凑尺寸别名，
 * 避免迁移业务组件时生成第二套按钮实现。
 */
export function Button({
    ref,
    className,
    variant = "default",
    size = "default",
    type = "button",
    ...props
  }: ButtonProps) {
  return (
    <button
      ref={ref}
      type={type}
      data-design-system-allow
      data-slot="button"
      data-variant={variant}
      data-size={size}
      className={cn(baseClass, variantClass[variant], sizeClass[size], className)}
      {...props}
    />
  );
}

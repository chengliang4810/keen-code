import {
  Card as AppicaCard,
  CardDescription as AppicaCardDescription,
  CardFooter as AppicaCardFooter,
  CardHeader as AppicaCardHeader,
  CardMedia as AppicaCardMedia,
  CardTitle as AppicaCardTitle,
  type CardDescriptionProps as AppicaCardDescriptionProps,
  type CardFooterProps as AppicaCardFooterProps,
  type CardHeaderProps as AppicaCardHeaderProps,
  type CardMediaProps as AppicaCardMediaProps,
  type CardProps as AppicaCardProps,
  type CardTitleProps as AppicaCardTitleProps,
} from "@appica/ui-react/card";

import { cn } from "@/lib/utils";

export type CardSize = "default" | "sm" | "md";

export interface CardProps extends AppicaCardProps {
  size?: CardSize;
}

/**
 * 共享卡片入口。Appica 继续负责 inset/frame 与 render 原语，
 * 这里集中锁定 KeenCode/ZCode 的卡片表面和紧凑尺寸。
 */
export function Card({
  className,
  contentProps,
  frame = false,
  inset = false,
  size = "md",
  ...props
}: CardProps) {
  const effectiveSize = size === "default" ? "md" : size;
  const contentClassName = frame === false ? "border-border bg-card" : undefined;

  return (
    <AppicaCard
      {...props}
      frame={frame}
      inset={inset}
      data-size={effectiveSize}
      className={cn("text-foreground", className)}
      contentProps={{
        ...contentProps,
        className: cn(
          contentClassName,
          effectiveSize === "sm" ? "gap-3 py-3" : "gap-4 py-4",
          contentProps?.className,
        ),
      }}
    />
  );
}

export function CardMedia({ className, ...props }: AppicaCardMediaProps) {
  return <AppicaCardMedia className={cn("overflow-hidden", className)} {...props} />;
}

export function CardHeader({ className, ...props }: AppicaCardHeaderProps) {
  return <AppicaCardHeader className={cn("gap-1.5", className)} {...props} />;
}

export function CardTitle({ className, ...props }: AppicaCardTitleProps) {
  return <AppicaCardTitle className={cn("text-sm font-medium text-foreground", className)} {...props} />;
}

export function CardDescription({ className, ...props }: AppicaCardDescriptionProps) {
  return <AppicaCardDescription className={cn("text-sm text-muted-foreground", className)} {...props} />;
}

export function CardFooter({ className, ...props }: AppicaCardFooterProps) {
  return <AppicaCardFooter className={cn("gap-2", className)} {...props} />;
}

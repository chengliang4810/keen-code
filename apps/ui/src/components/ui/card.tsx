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

/**
 * 共享卡片入口。Appica 负责 inset/frame、插槽间距与 render 原语；
 * 这里只锁定 KeenCode/ZCode 的卡片表面（无 frame 时的 border + bg-card），
 * 不再覆盖官方几何（内容层与插槽的内边距、间距由官方 Card 提供）。
 */
export function Card({
  className,
  contentProps,
  frame = false,
  inset = false,
  ...props
}: AppicaCardProps) {
  const contentClassName = frame === false ? "border-border bg-card" : undefined;

  return (
    <AppicaCard
      {...props}
      frame={frame}
      inset={inset}
      className={cn("text-foreground", className)}
      contentProps={{
        ...contentProps,
        className: cn(
          contentClassName,
          contentProps?.className,
        ),
      }}
    />
  );
}

export function CardMedia({ className, ...props }: AppicaCardMediaProps) {
  return <AppicaCardMedia className={cn(className)} {...props} />;
}

export function CardHeader({ className, ...props }: AppicaCardHeaderProps) {
  return <AppicaCardHeader className={cn(className)} {...props} />;
}

export function CardTitle({ className, ...props }: AppicaCardTitleProps) {
  return <AppicaCardTitle className={cn("text-sm font-medium text-foreground", className)} {...props} />;
}

export function CardDescription({ className, ...props }: AppicaCardDescriptionProps) {
  return <AppicaCardDescription className={cn("text-sm text-muted-foreground", className)} {...props} />;
}

export function CardFooter({ className, ...props }: AppicaCardFooterProps) {
  return <AppicaCardFooter className={cn(className)} {...props} />;
}

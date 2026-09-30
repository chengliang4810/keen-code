import {
  Button as AppicaButton,
  type ButtonProps as AppicaButtonProps,
} from "@appica/ui-react/button";

import { cn } from "@/lib/utils";

/**
 * Appica 的 md 尺寸固定 text-sm（13px + delta）、icon 系尺寸无字号声明（继承容器），
 * 都不满足 DESIGN.md「常规按钮文字 = --text-md」。这里按尺寸补 text-ui-base
 * （= --ui-font-size，界面字号），使常规与中号图标按钮文字恒等于界面字号；
 * sm / lg / icon-sm / icon-lg 保留 Appica 官方字号语义。
 * 未显式传 size 时按官方默认 md 注入；若未来在 ButtonGroup 中依赖组尺寸继承
 * 且组尺寸不是 md，请显式传 size。
 */
const fontSizeBySize: Partial<Record<NonNullable<AppicaButtonProps["size"]>, string>> = {
  md: "text-ui-base",
  "icon-md": "text-ui-base",
};

export type ButtonProps = AppicaButtonProps;

export function Button({ size, className, ...props }: AppicaButtonProps) {
  return (
    <AppicaButton
      size={size}
      className={cn(fontSizeBySize[size ?? "md"], className)}
      {...props}
    />
  );
}

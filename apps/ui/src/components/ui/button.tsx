import {
  Button as AppicaButton,
  type ButtonProps as AppicaButtonProps,
} from "@appica/ui-react/button";

import { cn } from "@/lib/utils";

/**
 * Appica 的 md 尺寸固定 text-sm（13px + delta）、icon 系尺寸无字号声明（继承容器），
 * 都不满足 DESIGN.md「常规按钮文字 = --text-md」。这里按尺寸补 button-type-md
 * （apps/ui/src/styles/ui-governance.css，消费 --text-md/--leading-normal）。
 *
 * 不能使用 Tailwind 字号类（如 text-ui-base）：Appica Button 内部会用未配置本
 * 项目 @theme 的 tailwind-merge 再次合并 className，把 text-ui-base 当作文字
 * 颜色类，进而删除 text-primary-foreground 等官方前景色类，导致按钮文字回归
 * 继承色而不可读（2026-09-30 bd5a4328 引入的回归）。普通 CSS 类不在
 * tailwind-merge 的认知范围内，会被原样保留。
 *
 * sm / lg / icon-sm / icon-lg 保留 Appica 官方字号语义。未显式传 size 时按官方
 * 默认 md 注入；若未来在 ButtonGroup 中依赖组尺寸继承且组尺寸不是 md，请显式传 size。
 */
const fontSizeClassBySize: Partial<
  Record<NonNullable<AppicaButtonProps["size"]>, string>
> = {
  md: "button-type-md",
  "icon-md": "button-type-md",
};

export type ButtonProps = AppicaButtonProps;

export function Button({ size, className, ...props }: AppicaButtonProps) {
  return (
    <AppicaButton
      size={size}
      className={cn(fontSizeClassBySize[size ?? "md"], className)}
      {...props}
    />
  );
}

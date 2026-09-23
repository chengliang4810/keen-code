import * as React from "react";
import {
  Input as AppicaInput,
  type InputProps as AppicaInputProps,
} from "@appica/ui-react/input";

import { cn } from "@/lib/utils";

/**
 * 共享输入框入口。Appica 负责 inputSize 几何（md 默认高度、内边距、圆角与
 * 图标尺寸）；这里只保留 KeenCode 的输入表面色、焦点状态与产品布局类。
 */
export interface InputProps
  extends Omit<AppicaInputProps, "inputSize" | "size"> {
  htmlSize?: number;
  ref?: React.Ref<HTMLInputElement>;
}

export function Input({
  ref,
  className,
  htmlSize,
  ...props
}: InputProps) {
  return (
    <AppicaInput
      ref={ref}
      inputSize="md"
      inputProps={htmlSize === undefined ? undefined : { size: htmlSize }}
      data-slot="input"
      className={cn(
        "w-full min-w-0 border-input-border bg-input text-foreground text-ui-base transition-colors outline-none file:inline-flex file:border-0 file:bg-transparent file:font-medium file:text-foreground placeholder:text-foreground-subtlest hover:border-input-border-hover focus-visible:border-input-border-focused focus-visible:bg-input-focused focus-visible:ring-0 disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-2 aria-invalid:ring-destructive/20 dark:aria-invalid:border-destructive/50 dark:aria-invalid:ring-destructive/40",
        className,
      )}
      {...props}
    />
  );
}

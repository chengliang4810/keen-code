import * as React from "react";
import {
  Input as AppicaInput,
  type InputProps as AppicaInputProps,
} from "@appica/ui-react/input";

import { cn } from "@/lib/utils";

/**
 * ZCode UI（Apache-2.0，归属见 THIRD_PARTY_NOTICES.md）的紧凑输入框几何。
 * 交互与无障碍实现继续由已安装的 Appica 原语提供。
 */
const inputSizeClass = {
  xs: "h-5 rounded-sm px-2 py-0.5 text-ui-base file:h-4 file:text-ui-base",
  sm: "h-6 rounded-md px-2 py-0.5 text-ui-base/relaxed file:h-5 file:text-ui-base/relaxed",
  default:
    "keencode-control-md h-7 rounded-md px-2 py-0.5 text-ui-base md:text-ui-base/relaxed file:h-6 file:text-ui-base/relaxed",
  md: "keencode-control-md h-7 rounded-md px-2 py-0.5 text-ui-base md:text-ui-base/relaxed file:h-6 file:text-ui-base/relaxed",
  lg: "h-8 rounded-lg px-3 py-1.5 text-ui-base file:h-6 file:text-ui-base",
} as const;

export type InputSize = keyof typeof inputSizeClass;

export interface InputProps
  extends Omit<AppicaInputProps, "inputSize" | "size"> {
  size?: InputSize;
  htmlSize?: number;
  ref?: React.Ref<HTMLInputElement>;
}

export function Input({
  ref,
  className,
  htmlSize,
  size = "md",
  ...props
}: InputProps) {
  return (
    <AppicaInput
      ref={ref}
      inputSize="md"
      inputProps={htmlSize === undefined ? undefined : { size: htmlSize }}
      data-slot="input"
      className={cn(
        "w-full min-w-0 border border-input-border bg-input text-foreground transition-colors outline-none file:inline-flex file:border-0 file:bg-transparent file:font-medium file:text-foreground placeholder:text-foreground-subtlest hover:border-input-border-hover focus-visible:border-input-border-focused focus-visible:bg-input-focused focus-visible:ring-0 disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-2 aria-invalid:ring-destructive/20 dark:aria-invalid:border-destructive/50 dark:aria-invalid:ring-destructive/40",
        inputSizeClass[size],
        className,
      )}
      {...props}
    />
  );
}

export { inputSizeClass as inputVariants };

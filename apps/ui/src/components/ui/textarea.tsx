import * as React from "react";
import {
  Textarea as AppicaTextarea,
  type TextareaProps as AppicaTextareaProps,
} from "@appica/ui-react/textarea";

import { cn } from "@/lib/utils";

/**
 * 共享文本域入口。Appica 的 inputSize（md）负责最小高度、内边距与文本缩放，
 * rows 与自动高度由官方计算；这里只保留 KeenCode 的正文字号。
 */
export interface TextareaProps extends Omit<AppicaTextareaProps, "inputSize"> {
  /** Appica 文本域统一使用中号；调用方不能覆盖底层尺寸。 */
  inputSize?: "md";
  ref?: React.Ref<HTMLTextAreaElement>;
}

export function Textarea({ className, inputSize: _inputSize, ...props }: TextareaProps) {
  return (
    <AppicaTextarea
      inputSize="md"
      className={cn("text-ui-base md:text-ui-base/relaxed", className)}
      {...props}
    />
  );
}

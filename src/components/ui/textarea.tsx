import * as React from "react";
import {
  Textarea as AppicaTextarea,
  type TextareaProps as AppicaTextareaProps,
} from "@appica/ui-react/textarea";

import { cn } from "@/lib/utils";

/**
 * ZCode 的文本域基线为 min-h-16、px-2、py-2；Appica 仍负责字段状态、插槽
 * 和无障碍行为，但统一从本地入口显式锁定 md 原语尺寸。
 */
export interface TextareaProps extends Omit<AppicaTextareaProps, "inputSize"> {
  inputSize?: "md";
  ref?: React.Ref<HTMLTextAreaElement>;
}

export function Textarea({ className, inputSize: _inputSize, ...props }: TextareaProps) {
  return (
    <AppicaTextarea
      inputSize="md"
      className={cn(
        "keencode-textarea min-h-16 px-2 py-2 text-ui-base md:text-ui-base/relaxed",
        className,
      )}
      {...props}
    />
  );
}

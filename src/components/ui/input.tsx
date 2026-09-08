import * as React from "react"

import { cn } from "@/lib/utils"

type InputProps = React.ComponentProps<"input"> & {
  variant?: "settings" | "compact";
};

function Input({ className, type, variant, ...props }: InputProps) {
  return (
    <input
      type={type}
      data-slot="input"
      className={cn(variant === "settings" && "settings-input", variant === "compact" && "prov-model-row__context", className)}
      {...props}
    />
  )
}

export { Input }

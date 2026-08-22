import * as React from "react"
import { Slot } from "@radix-ui/react-slot"

import { cn } from "@/lib/utils"

/** 统一按钮语义与 Slot 组合，不覆盖各业务控件既有的盒模型。 */
function Button({
  className,
  asChild = false,
  ...props
}: React.ComponentProps<"button"> & { asChild?: boolean }) {
  const Comp = asChild ? Slot : "button"

  return (
    <Comp
      data-slot="button"
      className={cn(className)}
      {...props}
    />
  )
}

export { Button }

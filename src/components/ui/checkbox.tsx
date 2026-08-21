import * as React from "react"
import * as CheckboxPrimitive from "@radix-ui/react-checkbox"

import { IconCheck } from "@/components/icons"
import { cn } from "@/lib/utils"

function Checkbox({
  className,
  ...props
}: React.ComponentProps<typeof CheckboxPrimitive.Root>) {
  return (
    <CheckboxPrimitive.Root
      data-slot="checkbox"
      className={cn(
        "peer size-4 shrink-0 rounded-[4px] border border-[var(--border-strong)] bg-[var(--bg-input)] text-[var(--accent-fg)] shadow-xs transition-[color,background-color,border-color,box-shadow] duration-[var(--motion-fast)] outline-none focus-visible:border-[var(--border-focus)] focus-visible:ring-2 focus-visible:ring-ring/40 disabled:cursor-not-allowed disabled:opacity-45 aria-invalid:border-[var(--danger)] aria-invalid:ring-[var(--danger)]/20 data-[state=checked]:border-[var(--accent)] data-[state=checked]:bg-[var(--accent)] data-[state=checked]:text-[var(--accent-fg)] data-[state=indeterminate]:border-[var(--accent)] data-[state=indeterminate]:bg-[var(--accent)] data-[state=indeterminate]:text-[var(--accent-fg)]",
        className,
      )}
      {...props}
    >
      <CheckboxPrimitive.Indicator
        data-slot="checkbox-indicator"
        className="grid place-content-center text-current transition-none"
      >
        <IconCheck size={13} stroke={2} />
      </CheckboxPrimitive.Indicator>
    </CheckboxPrimitive.Root>
  )
}

export { Checkbox }

import * as React from "react"
import * as ToggleGroupPrimitive from "@radix-ui/react-toggle-group"

import { cn } from "@/lib/utils"

type ToggleGroupVariant = "default" | "outline"
type ToggleGroupSize = "sm" | "default" | "lg"

type ToggleGroupContextValue = {
  variant: ToggleGroupVariant
  size: ToggleGroupSize
  spacing: number
}

const ToggleGroupContext = React.createContext<ToggleGroupContextValue>({
  variant: "default",
  size: "default",
  spacing: 0,
})

type ToggleGroupProps = React.ComponentProps<typeof ToggleGroupPrimitive.Root> & {
  variant?: ToggleGroupVariant
  size?: ToggleGroupSize
  spacing?: number
}

function ToggleGroup({
  className,
  variant = "default",
  size = "default",
  spacing = 0,
  children,
  style,
  ...props
}: ToggleGroupProps) {
  return (
    <ToggleGroupPrimitive.Root
      data-slot="toggle-group"
      data-variant={variant}
      data-size={size}
      data-spacing={spacing}
      style={
        {
          "--toggle-group-gap": `${spacing}px`,
          ...style,
        } as React.CSSProperties
      }
      className={cn(
        "group/toggle-group flex w-fit items-center gap-[var(--toggle-group-gap)] rounded-[var(--radius-md)] data-[spacing=0]:data-[variant=outline]:shadow-xs",
        className,
      )}
      {...props}
    >
      <ToggleGroupContext.Provider value={{ variant, size, spacing }}>
        {children}
      </ToggleGroupContext.Provider>
    </ToggleGroupPrimitive.Root>
  )
}

type ToggleGroupItemProps =
  React.ComponentProps<typeof ToggleGroupPrimitive.Item> & {
    variant?: ToggleGroupVariant
    size?: ToggleGroupSize
  }

function ToggleGroupItem({
  className,
  children,
  variant,
  size,
  ...props
}: ToggleGroupItemProps) {
  const context = React.useContext(ToggleGroupContext)
  const resolvedVariant = variant ?? context.variant
  const resolvedSize = size ?? context.size

  return (
    <ToggleGroupPrimitive.Item
      data-slot="toggle-group-item"
      data-variant={resolvedVariant}
      data-size={resolvedSize}
      data-spacing={context.spacing}
      className={cn(
        "inline-flex w-auto min-w-0 shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-[var(--radius-sm)] border border-transparent text-[length:var(--text-sm)] font-medium text-[var(--text-primary)] outline-none transition-[color,background-color,border-color,box-shadow] duration-[var(--motion-fast)] hover:bg-[var(--bg-hover)] focus-visible:z-10 focus-visible:border-[var(--border-focus)] focus-visible:ring-2 focus-visible:ring-ring/40 disabled:pointer-events-none disabled:opacity-45 data-[state=on]:bg-[var(--accent-muted)] data-[state=on]:text-[var(--text-primary)] data-[variant=outline]:border-[var(--border-subtle)] data-[variant=outline]:data-[state=on]:border-[var(--accent)] data-[variant=outline]:data-[state=on]:bg-[var(--bg-active)] data-[spacing=0]:rounded-none data-[spacing=0]:shadow-none data-[spacing=0]:first:rounded-l-[var(--radius-md)] data-[spacing=0]:last:rounded-r-[var(--radius-md)] data-[spacing=0]:data-[variant=outline]:border-l-0 data-[spacing=0]:data-[variant=outline]:first:border-l",
        resolvedSize === "sm" && "h-7 px-2 text-xs",
        resolvedSize === "lg" && "h-9 px-3",
        resolvedSize === "default" &&
          "h-[var(--control-height)] px-2.5",
        className,
      )}
      {...props}
    >
      {children}
    </ToggleGroupPrimitive.Item>
  )
}

export { ToggleGroup, ToggleGroupItem }

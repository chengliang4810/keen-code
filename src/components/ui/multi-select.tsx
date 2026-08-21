import * as React from "react"

import { IconChevronDown } from "@/components/icons"
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"

export type MultiSelectOption = {
  value: string
  label: string
  disabled?: boolean
}

export interface MultiSelectProps {
  options: readonly MultiSelectOption[]
  value: readonly string[]
  onValueChange: (value: string[]) => void
  placeholder: string
  ariaLabel: string
  ariaDescribedBy?: string
  disabled?: boolean
  className?: string
  renderValue?: (
    selected: readonly MultiSelectOption[],
  ) => React.ReactNode
}

function uniqueOptions(options: readonly MultiSelectOption[]) {
  const seen = new Set<string>()

  return options.filter((option) => {
    if (seen.has(option.value)) return false
    seen.add(option.value)
    return true
  })
}

function defaultRenderValue(
  selected: readonly MultiSelectOption[],
  placeholder: string,
) {
  if (selected.length === 0) return placeholder
  return selected.map((option) => option.label).join(", ")
}

function focusNextTabStop(trigger: HTMLButtonElement, backwards: boolean) {
  const focusScope =
    trigger.closest<HTMLElement>("[role='dialog'], [role='alertdialog']") ??
    document
  const focusableSelector = [
    "a[href]",
    "button:not([disabled])",
    "input:not([disabled])",
    "textarea:not([disabled])",
    "[tabindex]:not([tabindex='-1'])",
  ].join(",")
  const tabStops = Array.from(
    focusScope.querySelectorAll<HTMLElement>(focusableSelector),
  ).filter((element) => {
    if (element.closest("[inert], [aria-hidden='true']")) return false
    if (element.hasAttribute("hidden")) return false
    return element.getClientRects().length > 0
  })
  const triggerIndex = tabStops.indexOf(trigger)
  if (triggerIndex < 0 || tabStops.length === 0) {
    trigger.focus()
    return
  }
  const nextIndex =
    (triggerIndex + (backwards ? -1 : 1) + tabStops.length) % tabStops.length
  const next = tabStops[nextIndex]

  if (next) {
    next.focus()
  } else {
    trigger.focus()
  }
}

export function MultiSelect({
  options,
  value,
  onValueChange,
  placeholder,
  ariaLabel,
  ariaDescribedBy,
  disabled = false,
  className,
  renderValue,
}: MultiSelectProps) {
  const [open, setOpen] = React.useState(false)
  const triggerRef = React.useRef<HTMLButtonElement>(null)
  const normalizedOptions = React.useMemo(() => uniqueOptions(options), [options])
  const selectedValueSet = React.useMemo(() => new Set(value), [value])
  const selectedOptions = React.useMemo(
    () => normalizedOptions.filter((option) => selectedValueSet.has(option.value)),
    [normalizedOptions, selectedValueSet],
  )
  const selectedOptionValues = React.useMemo(
    () => new Set(selectedOptions.map((option) => option.value)),
    [selectedOptions],
  )

  const handleToggle = React.useCallback(
    (option: MultiSelectOption) => {
      if (disabled || option.disabled) return

      const nextSelectedValues = new Set(selectedOptionValues)
      if (nextSelectedValues.has(option.value)) {
        nextSelectedValues.delete(option.value)
      } else {
        nextSelectedValues.add(option.value)
      }

      onValueChange(
        normalizedOptions
          .filter((candidate) => nextSelectedValues.has(candidate.value))
          .map((candidate) => candidate.value),
      )
    },
    [disabled, normalizedOptions, onValueChange, selectedOptionValues],
  )

  const renderedValue = renderValue
    ? renderValue(selectedOptions)
    : defaultRenderValue(selectedOptions, placeholder)

  const handleContentKeyDown = React.useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      if (event.key !== "Tab") return

      event.preventDefault()
      setOpen(false)

      const trigger = triggerRef.current
      if (!trigger) return

      window.requestAnimationFrame(() => {
        focusNextTabStop(trigger, event.shiftKey)
      })
    },
    [],
  )

  return (
    <DropdownMenu
      data-slot="multi-select"
      open={open}
      onOpenChange={setOpen}
    >
      <DropdownMenuTrigger asChild data-slot="multi-select-trigger">
        <button
          type="button"
          aria-label={ariaLabel}
          aria-describedby={ariaDescribedBy}
          data-slot="multi-select-trigger"
          ref={triggerRef}
          disabled={disabled}
          className={cn(
            "inline-flex min-w-0 items-center justify-between gap-2 rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--bg-input)] px-2.5 text-[length:var(--text-sm)] leading-none text-[var(--text-primary)] outline-none transition-[background-color,border-color,box-shadow] duration-[var(--motion-fast)] ease-out hover:border-[var(--border-strong)] hover:bg-[var(--bg-hover)] focus-visible:border-[var(--border-focus)] focus-visible:ring-2 focus-visible:ring-ring/40 disabled:cursor-not-allowed disabled:opacity-45",
            className,
          )}
        >
          <span
            data-slot="multi-select-value"
            className="min-w-0 truncate text-left"
          >
            {renderedValue}
          </span>
          <IconChevronDown
            aria-hidden="true"
            size={14}
            className="shrink-0 opacity-60"
          />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        data-slot="multi-select-content"
        align="start"
        onKeyDown={handleContentKeyDown}
        className="max-h-72 min-w-48 max-w-[calc(100vw-2rem)] overflow-y-auto rounded-[var(--menu-radius)] border-[var(--menu-border)] bg-[var(--menu-surface)] p-[var(--menu-pad)] text-[length:var(--menu-font)] text-[var(--text-primary)] shadow-[var(--menu-shadow)] backdrop-blur-[var(--menu-blur)]"
      >
        <DropdownMenuGroup>
          {normalizedOptions.map((option) => (
            <DropdownMenuCheckboxItem
              key={option.value}
              data-slot="multi-select-item"
              checked={selectedOptionValues.has(option.value)}
              disabled={disabled || option.disabled}
              className="text-[length:var(--menu-font)] leading-[1.3] text-[var(--text-primary)] focus:bg-[var(--menu-hover)] focus:text-[var(--text-primary)] data-[highlighted]:bg-[var(--menu-hover)] data-[highlighted]:text-[var(--text-primary)] data-[state=checked]:bg-[var(--menu-active)] [&>span]:text-[var(--accent)]"
              onSelect={(event) => {
                event.preventDefault()
                handleToggle(option)
              }}
            >
              {option.label}
            </DropdownMenuCheckboxItem>
          ))}
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

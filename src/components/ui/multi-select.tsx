import type { ReactNode } from "react";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@appica/ui-react/select";

export type MultiSelectOption = {
  value: string;
  label: ReactNode;
  disabled?: boolean;
};

export interface MultiSelectProps {
  options: readonly MultiSelectOption[];
  value: readonly string[];
  onValueChange: (value: string[]) => void;
  placeholder: string;
  ariaLabel: string;
  ariaDescribedBy?: string;
  disabled?: boolean;
  className?: string;
  renderValue?: (selected: readonly MultiSelectOption[]) => ReactNode;
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
  const uniqueOptions = Array.from(new Map(options.map((option) => [option.value, option])).values());
  const optionMap = new Map(uniqueOptions.map((option) => [option.value, option]));
  const requestedValues = new Set(value);
  const selectedValues = uniqueOptions
    .filter((option) => requestedValues.has(option.value))
    .map((option) => option.value);
  const selectedOptions = selectedValues.map((entry) => optionMap.get(entry)!);

  const summary = renderValue
    ? renderValue(selectedOptions)
    : selectedOptions.map((option) => option.label).join(", ");

  return (
    <Select
      multiple
      value={selectedValues}
      disabled={disabled}
      onValueChange={(next) => onValueChange(Array.isArray(next) ? next.filter((entry): entry is string => typeof entry === "string") : [])}
    >
      <SelectTrigger
        className={className}
        aria-label={ariaLabel}
        aria-describedby={ariaDescribedBy}
      >
        <SelectValue placeholder={placeholder}>{() => summary || placeholder}</SelectValue>
      </SelectTrigger>
      <SelectContent>
        {uniqueOptions.map((option) => (
          <SelectItem key={option.value} value={option.value} disabled={option.disabled}>
            {option.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

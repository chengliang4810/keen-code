import { Input, type InputProps } from "@/components/ui/input";
import { cn } from "@/lib/utils";

interface SettingsNumberInputProps extends Omit<InputProps, "type" | "value" | "defaultValue" | "onBlur" | "onChange" | "onKeyDown"> {
  value: number;
  min: number;
  max: number;
  onCommit: (value: number) => void;
}

/** 数字设置失焦或回车时提交；无效草稿恢复已保存值，不让界面显示未生效的数字。 */
export function SettingsNumberInput({ value, min, max, onCommit, className, ...props }: SettingsNumberInputProps) {
  return (
    <Input
      key={value}
      {...props}
      className={cn("settings-number-input", className)}
      type="number"
      inputMode="numeric"
      min={min}
      max={max}
      step={1}
      defaultValue={value}
      onKeyDown={(event) => {
        if (event.key === "Enter") event.currentTarget.blur();
      }}
      onBlur={(event) => {
        const raw = event.currentTarget.value;
        const next = Number(raw);
        if (!raw.trim() || !Number.isInteger(next) || next < min || next > max) {
          event.currentTarget.value = String(value);
          return;
        }
        if (next !== value) onCommit(next);
      }}
    />
  );
}

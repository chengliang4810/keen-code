import {
  Checkbox as AppicaCheckbox,
  type CheckboxProps as AppicaCheckboxProps,
} from "@appica/ui-react/checkbox";
import {
  CheckboxGroup as AppicaCheckboxGroup,
  type CheckboxGroupProps as AppicaCheckboxGroupProps,
} from "@appica/ui-react/checkbox-group";

import { cn } from "@/lib/utils";

/**
 * 共享多选组入口。Appica 负责控件几何与勾选指示器；
 * 带文案的选项行按官方模式用 <label> 包裹 Checkbox 与文字。
 */
export function CheckboxGroup({ className, ...props }: AppicaCheckboxGroupProps) {
  return <AppicaCheckboxGroup className={cn("gap-2", className)} {...props} />;
}

export function Checkbox({ className, ...props }: AppicaCheckboxProps) {
  return <AppicaCheckbox className={cn("shrink-0", className)} {...props} />;
}

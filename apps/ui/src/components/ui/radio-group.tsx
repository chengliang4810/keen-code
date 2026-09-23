import {
  Radio as AppicaRadio,
  type RadioProps as AppicaRadioProps,
} from "@appica/ui-react/radio";
import {
  RadioGroup as AppicaRadioGroup,
  type RadioGroupProps as AppicaRadioGroupProps,
} from "@appica/ui-react/radio-group";

import { cn } from "@/lib/utils";

/**
 * 共享单选组入口。Appica 负责控件几何、键盘漫游与选中指示器；
 * 带文案的选项行按官方模式用 <label> 包裹 Radio 与文字。
 */
export function RadioGroup({ className, ...props }: AppicaRadioGroupProps) {
  return <AppicaRadioGroup className={cn("gap-2", className)} {...props} />;
}

export function Radio({ className, ...props }: AppicaRadioProps) {
  return <AppicaRadio className={cn("shrink-0", className)} {...props} />;
}

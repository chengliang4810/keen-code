import {
  Switch as AppicaSwitch,
  type SwitchProps as AppicaSwitchProps,
} from "@appica/ui-react/switch";

import { cn } from "@/lib/utils";

/**
 * 共享 Switch 入口。Appica 的 size（md）负责轨道与滑块几何及颜色状态；
 * 这里只保留 KeenCode 的扩展点击热区与未选中轨道色。
 */
export function Switch({ className, ...props }: AppicaSwitchProps) {
  return (
    <AppicaSwitch
      size="md"
      data-slot="switch"
      className={cn(
        "peer relative after:absolute after:-inset-x-3 after:-inset-y-2 data-unchecked:bg-primary/30",
        className,
      )}
      {...props}
    />
  );
}

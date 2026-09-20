import {
  Switch as AppicaSwitch,
  type SwitchProps,
} from "@appica/ui-react/switch";

function Switch({ size = "md", ...props }: SwitchProps) {
  return <AppicaSwitch size={size} {...props} />;
}

export { Switch };
export type { SwitchProps };

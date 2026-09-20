import {
  Button as AppicaButton,
  type ButtonProps,
} from "@appica/ui-react/button";

function Button({ size = "md", ...props }: ButtonProps) {
  return <AppicaButton size={size} {...props} />;
}

export { Button };
export type { ButtonProps };

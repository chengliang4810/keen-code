import { Spinner as BaseSpinner } from "@/components/ui/spinner";
import { useTranslation } from "@/modules/i18n";
import type { ComponentProps } from "react";

export function Spinner(props: ComponentProps<typeof BaseSpinner>) {
  const tr = useTranslation();
  return (
    <BaseSpinner {...props} aria-label={props["aria-label"] ?? tr("Loading")} />
  );
}

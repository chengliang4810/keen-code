import { Button } from "@/components/ui/button.js";
import { X } from "lucide-react";

/** 单页偏好引导不再显示职业/模式步骤或返回上一页。 */
export function OnboardingHeader({ saving, t, onClose }: {
  saving: boolean;
  t: (key: string) => string;
  onClose: () => void;
}) {
  return (
    <header className="relative grid h-14 shrink-0 grid-cols-[1fr_auto_1fr] [@media(max-height:740px)]:h-10 items-center px-6 sm:px-10">
      <Button variant="ghost" size="icon" disabled={saving} aria-label={t("close")} title={t("close")}
        className="col-start-3 row-start-1 justify-self-end size-9 rounded-xl text-foreground-subtle" onClick={onClose}>
        <X className="size-4" />
      </Button>
    </header>
  );
}

import { cn } from "@/lib/utils";
import { ControlLabelContext } from "@/components/ui/controlLabel";
import { type ReactNode, useId } from "react";

type Props = {
  title: ReactNode;
  description?: string;
  children: React.ReactNode;
  className?: string;
};

export function SettingRow({ title, description, children, className }: Props) {
  const labelId = useId();
  return (
    <div
      data-setting-row
      className={cn(
        "flex items-start justify-between gap-4 rounded-lg border border-border/60 bg-card/60 px-3 py-2.5",
        className,
      )}
    >
      <div className="flex min-w-0 flex-col gap-0.5">
        <span id={labelId} className="text-ui-base font-medium">{title}</span>
        {description ? (
          <span className="text-ui-sm leading-relaxed text-muted-foreground">
            {description}
          </span>
        ) : null}
      </div>
      <ControlLabelContext value={labelId}>
        <div className="flex shrink-0 items-center">{children}</div>
      </ControlLabelContext>
    </div>
  );
}

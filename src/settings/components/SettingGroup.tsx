import type { ReactNode } from "react";

export function SettingGroup({
  title,
  children,
}: {
  title: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="flex flex-col gap-3">
      <h3 className="text-ui-base font-medium">{title}</h3>
      <div
        data-setting-group
        className="rounded-xl border border-border/60 bg-card/60 px-4"
      >
        {children}
      </div>
    </section>
  );
}

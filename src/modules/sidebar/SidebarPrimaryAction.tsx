import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

export function SidebarPrimaryAction({
  children,
  onClick,
  disabled,
  muted = false,
}: {
  children: ReactNode;
  onClick: () => void;
  disabled?: boolean;
  muted?: boolean;
}) {
  return (
    <div className="shrink-0 px-2 pt-2 pb-3">
      <Button
        variant="ghost"
        className={cn(
          "h-9 w-full justify-start gap-2 rounded-md px-2 text-ui-base",
          muted && "font-normal text-muted-foreground hover:text-foreground",
        )}
        onClick={onClick}
        disabled={disabled}
      >
        {children}
      </Button>
    </div>
  );
}

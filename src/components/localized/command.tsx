import { useTranslation } from "@/modules/i18n";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/localized/dialog";
import type { CommandDialog as BaseCommandDialog } from "@/components/ui/command";
import { cn } from "@/lib/utils";
import type { ComponentProps } from "react";

export * from "@/components/ui/command";

export function CommandDialog({
  title = "Command Palette",
  description = "Search for a command to run...",
  children,
  className,
  showCloseButton = false,
  ...props
}: ComponentProps<typeof BaseCommandDialog>) {
  const tr = useTranslation();
  return (
    <Dialog {...props}>
      <DialogHeader className="sr-only">
        <DialogTitle>{tr(title)}</DialogTitle>
        <DialogDescription>{tr(description)}</DialogDescription>
      </DialogHeader>
      <DialogContent
        className={cn(
          "top-1/3 translate-y-0 overflow-hidden rounded-4xl! p-0",
          className,
        )}
        showCloseButton={showCloseButton}
      >
        {children}
      </DialogContent>
    </Dialog>
  );
}

import {
  Dialog as AppicaDialog,
  DialogBody as AppicaDialogBody,
  DialogClose as AppicaDialogClose,
  DialogContent as AppicaDialogContent,
  DialogDescription as AppicaDialogDescription,
  DialogFooter as AppicaDialogFooter,
  DialogHeader as AppicaDialogHeader,
  DialogTitle as AppicaDialogTitle,
  DialogTrigger as AppicaDialogTrigger,
  type DialogBodyProps as AppicaDialogBodyProps,
  type DialogCloseProps as AppicaDialogCloseProps,
  type DialogContentProps as AppicaDialogContentProps,
  type DialogDescriptionProps as AppicaDialogDescriptionProps,
  type DialogFooterProps as AppicaDialogFooterProps,
  type DialogHeaderProps as AppicaDialogHeaderProps,
  type DialogProps as AppicaDialogProps,
  type DialogTitleProps as AppicaDialogTitleProps,
  type DialogTriggerProps as AppicaDialogTriggerProps,
} from "@appica/ui-react/dialog";

import { cn } from "@/lib/utils";

/** The local dialog boundary keeps Appica's focus/portal behavior intact. */
export function Dialog(props: AppicaDialogProps) {
  return <AppicaDialog {...props} />;
}

export function DialogTrigger({ className, ...props }: AppicaDialogTriggerProps) {
  return <AppicaDialogTrigger className={cn(className)} {...props} />;
}

export function DialogContent({ className, frame = false, ...props }: AppicaDialogContentProps) {
  return (
    <AppicaDialogContent
      frame={frame}
      className={cn(
        "rounded-2xl border border-border shadow-2xl [&>[data-slot=dialog-content]]:overflow-hidden [&>[data-slot=dialog-content]]:rounded-[inherit]",
        className,
      )}
      {...props}
    />
  );
}

export function DialogHeader({ className, ...props }: AppicaDialogHeaderProps) {
  return <AppicaDialogHeader className={cn("gap-1 p-6", className)} {...props} />;
}

export function DialogTitle({ className, ...props }: AppicaDialogTitleProps) {
  return <AppicaDialogTitle className={cn("text-ui-base font-medium text-foreground", className)} {...props} />;
}

export function DialogDescription({ className, ...props }: AppicaDialogDescriptionProps) {
  return <AppicaDialogDescription className={cn("text-ui-base/relaxed text-foreground-subtle", className)} {...props} />;
}

export function DialogBody({ className, ...props }: AppicaDialogBodyProps) {
  return <AppicaDialogBody className={cn("min-h-0 flex-1 px-6", className)} {...props} />;
}

export function DialogFooter({ className, ...props }: AppicaDialogFooterProps) {
  return <AppicaDialogFooter className={cn("gap-2 p-6", className)} {...props} />;
}

export function DialogClose({ className, ...props }: AppicaDialogCloseProps) {
  return <AppicaDialogClose className={cn(className)} {...props} />;
}

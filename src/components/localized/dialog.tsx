import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import {
  DialogClose,
  DialogContent as BaseDialogContent,
  DialogFooter as BaseDialogFooter,
} from "@/components/ui/dialog";
import { Cancel01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import type { ComponentProps } from "react";

export * from "@/components/ui/dialog";

/** 业务层提供可翻译的关闭按钮，保留生成的 shadcn 基础组件。 */
export function DialogContent({
  children,
  showCloseButton = true,
  ...props
}: ComponentProps<typeof BaseDialogContent>) {
  const tr = useTranslation();
  return (
    <BaseDialogContent {...props} showCloseButton={false}>
      {children}
      {showCloseButton && (
        <DialogClose asChild>
          <Button
            variant="ghost"
            className="absolute top-4 right-4 bg-secondary"
            size="icon-sm"
          >
            <HugeiconsIcon icon={Cancel01Icon} strokeWidth={2} />
            <span className="sr-only">{tr("Close")}</span>
          </Button>
        </DialogClose>
      )}
    </BaseDialogContent>
  );
}

export function DialogFooter({
  children,
  showCloseButton = false,
  ...props
}: ComponentProps<typeof BaseDialogFooter>) {
  const tr = useTranslation();
  return (
    <BaseDialogFooter {...props} showCloseButton={false}>
      {children}
      {showCloseButton && (
        <DialogClose asChild>
          <Button variant="outline">{tr("Close")}</Button>
        </DialogClose>
      )}
    </BaseDialogFooter>
  );
}

import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@appica/ui-react/dialog";
import { useId, useRef, type ReactNode, type RefObject } from "react";

export type GlassModalSize = "sm" | "md" | "lg";

export type GlassModalProps = {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  size?: GlassModalSize;
  className?: string;
  overlayClassName?: string;
  bodyClassName?: string;
  wrapBody?: boolean;
  titleId?: string;
  closeLabel?: string;
  closeOnOverlay?: boolean;
  showClose?: boolean;
  returnFocusRef?: RefObject<HTMLElement | null>;
};

function cx(...parts: Array<string | false | null | undefined>) {
  return parts.filter(Boolean).join(" ");
}

/** KeenCode's product layout composed on Appica Dialog primitives. */
export function GlassModal({
  open,
  onClose,
  title,
  children,
  footer,
  size = "md",
  className,
  overlayClassName,
  bodyClassName,
  wrapBody = false,
  titleId: titleIdProp,
  closeLabel = "Close",
  closeOnOverlay = true,
  showClose = true,
  returnFocusRef,
}: GlassModalProps) {
  const autoId = useId();
  const titleId = titleIdProp || autoId;
  const panelRef = useRef<HTMLDivElement>(null);
  const sizeClass =
    size === "sm"
      ? "glass-modal--sm"
      : size === "lg"
        ? "glass-modal--lg"
        : "glass-modal--md";

  return (
    <Dialog
      open={open}
      disablePointerDismissal={!closeOnOverlay}
      onOpenChange={(nextOpen) => {
        if (!nextOpen) onClose();
      }}
    >
      <DialogContent
        ref={panelRef}
        frame={false}
        closeButton={showClose}
        closeLabel={closeLabel}
        className={cx("modal glass-modal", sizeClass, className)}
        viewportProps={{ className: overlayClassName }}
        initialFocus={() =>
          panelRef.current?.querySelector<HTMLElement>("[data-modal-autofocus]") ??
          true
        }
        finalFocus={returnFocusRef}
      >
        <DialogHeader className="modal-head">
          <DialogTitle id={titleId} className="modal-title">
            {title}
          </DialogTitle>
        </DialogHeader>

        {wrapBody || bodyClassName ? (
          <DialogBody className={cx("modal-body", bodyClassName)}>
            {children}
          </DialogBody>
        ) : (
          children
        )}

        {footer ? (
          <DialogFooter className="modal-actions">{footer}</DialogFooter>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

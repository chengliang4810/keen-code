import {
  ContextMenu as AppicaContextMenu,
  ContextMenuContent,
  ContextMenuItem as AppicaContextMenuItem,
  ContextMenuTrigger,
} from "@appica/ui-react/context-menu";
import { useEffect, useRef, type ReactNode } from "react";

export type ContextMenuItem = {
  id?: string;
  label: ReactNode;
  icon?: ReactNode;
  danger?: boolean;
  disabled?: boolean;
  onClick: () => void;
};

export type ContextMenuProps = {
  open: boolean;
  x: number;
  y: number;
  items: ContextMenuItem[];
  onClose: () => void;
  extra?: ReactNode;
};

/**
 * Appica Context Menu adapter for menus opened from coordinates stored in app state.
 * Appica owns collision handling, focus, keyboard navigation, dismissal and portals.
 */
export function ContextMenu({
  open,
  x,
  y,
  items,
  onClose,
  extra,
}: ContextMenuProps) {
  const triggerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const trigger = triggerRef.current;
    if (!trigger) return;
    trigger.dispatchEvent(
      new MouseEvent("contextmenu", {
        bubbles: true,
        cancelable: true,
        clientX: x,
        clientY: y,
        button: 2,
      }),
    );
  }, [open, x, y]);

  if (!open) return null;

  return (
    <AppicaContextMenu
      size="md"
      onOpenChange={(nextOpen) => {
        if (!nextOpen) onClose();
      }}
    >
      <ContextMenuTrigger
        ref={triggerRef}
        className="pointer-events-none fixed size-px opacity-0"
        render={<div aria-hidden />}
      />
      <ContextMenuContent>
        {items.map((item, index) => (
          <AppicaContextMenuItem
            key={item.id ?? `ctx-item-${index}`}
            className={
              item.danger
                ? "text-error-emphasis! data-highlighted:before:bg-error-subtle!"
                : undefined
            }
            disabled={item.disabled}
            onClick={item.onClick}
          >
            {item.icon != null ? (
              <span data-icon="start" aria-hidden>
                {item.icon}
              </span>
            ) : null}
            {item.label}
          </AppicaContextMenuItem>
        ))}
        {extra}
      </ContextMenuContent>
    </AppicaContextMenu>
  );
}

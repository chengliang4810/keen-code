import { Button } from "@appica/ui-react/button";
import { CopyButton } from "@appica/ui-react/copy-button";
/**
 * Compact chat hover action — KeenCode tip + optional copy→check feedback.
 */

import type { ReactNode } from "react";
import { Tip } from "@/components/ui/tooltip";

export function MessageActionButton({
  label,
  ariaLabel,
  onClick,
  disabled,
  children,
  className,
}: {
  label: string;
  ariaLabel?: string;
  onClick?: () => void;
  disabled?: boolean;
  children: ReactNode;
  className?: string;
}) {
  return (
    <Tip label={label} disabled={disabled || !label}>
      <Button
        type="button"
        variant="ghost"
        size="icon-md"
        className={className}
        aria-label={ariaLabel ?? label}
        disabled={disabled}
        onClick={onClick}
      >
        {children}
      </Button>
    </Tip>
  );
}

export function MessageCopyButton({
  text,
  copyLabel,
  copiedLabel = "OK",
}: {
  text: string;
  copyLabel: string;
  copiedLabel?: string;
}) {
  return (
    <CopyButton size="md" value={text} label={copyLabel} copiedLabel={copiedLabel} timeout={1200} />
  );
}

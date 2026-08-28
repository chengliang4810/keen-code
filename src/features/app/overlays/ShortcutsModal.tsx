import type { MessageKey } from "@/i18n";
import { shortcutsForPlatform } from "@/lib/shortcuts";
import { GlassModal } from "@/components/GlassModal";
import { Button } from "@/components/ui/button";
import type { SetState, Translator } from "./types";

export interface ShortcutsModalProps {
  tr: Translator;
  open: boolean;
  setOpen: SetState<boolean>;
  platform: string;
}

export function ShortcutsModal({
  tr,
  open,
  setOpen,
  platform,
}: ShortcutsModalProps) {
  const shortcutPlatform =
    platform === "mac" ? "mac" : platform === "win" ? "win" : "other";

  return (
    <GlassModal
      open={open}
      onClose={() => setOpen(false)}
      title={tr("shortcuts.title")}
      size="md"
      closeLabel={tr("shortcuts.close")}
      footer={
        <Button
          type="button"
          className="btn btn--ghost"
          onClick={() => setOpen(false)}
        >
          {tr("shortcuts.close")}
        </Button>
      }
    >
      <ul className="shortcuts-list">
        {shortcutsForPlatform(shortcutPlatform).map((row) => (
          <li key={row.id} className="shortcuts-list__row">
            <span className="shortcuts-list__label">
              {tr(row.labelKey as MessageKey)}
            </span>
            <kbd className="shortcuts-list__keys">{row.keys}</kbd>
          </li>
        ))}
      </ul>
    </GlassModal>
  );
}

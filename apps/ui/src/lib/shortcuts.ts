/** Keyboard shortcut catalog — help panel + Settings → Keyboard. */

export type ShortcutRow = {
  id: string;
  /** i18n message key for the action label */
  labelKey: string;
  /** Display keys for mac (⌘ is replaced at render time if needed) */
  mac: string;
  /** Display keys for win/linux */
  win: string;
};

/**
 * Shipped shortcuts that already work in the app.
 * Keep this list honest — only document real bindings.
 * Single source for the help modal and Settings → Keyboard.
 */
export const SHORTCUTS: ShortcutRow[] = [
  {
    id: "search",
    labelKey: "shortcuts.search",
    mac: "⌘ K",
    win: "Ctrl K",
  },
  {
    id: "newChat",
    labelKey: "shortcuts.newChat",
    mac: "⌘ N",
    win: "Ctrl N",
  },
  {
    id: "send",
    labelKey: "shortcuts.send",
    mac: "⌘ ↵",
    win: "Ctrl Enter",
  },
  {
    id: "stop",
    labelKey: "shortcuts.stop",
    mac: "Esc",
    win: "Esc",
  },
  {
    id: "settings",
    labelKey: "shortcuts.settings",
    mac: "⌘ ,",
    win: "Ctrl ,",
  },
  {
    id: "help",
    labelKey: "shortcuts.help",
    mac: "⌘ /",
    win: "Ctrl /",
  },
];

export function shortcutsForPlatform(
  platform: "mac" | "win" | "other",
): Array<{ id: string; labelKey: string; keys: string }> {
  return SHORTCUTS.map((s) => ({
    id: s.id,
    labelKey: s.labelKey,
    keys: platform === "mac" ? s.mac : s.win,
  }));
}

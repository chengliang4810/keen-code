import { IS_MAC, MOD_PROP } from "@/lib/platform";

/**
 * Single source of truth for keyboard shortcuts.
 */

export type ShortcutId =
  | "commandPalette.open"
  | "commandPalette.content"
  | "tab.new"
  | "tab.newPreview"
  | "tab.close"
  | "tab.next"
  | "tab.prev"
  | "tab.selectByIndex"
  | "space.next"
  | "space.prev"
  | "space.overview"
  | "terminal.clear"
  | "blocks.prev"
  | "blocks.next"
  | "search.focus"
  | "explorer.search"
  | "explorer.focus"
  | "explorer.toggleHidden"
  | "view.zoomIn"
  | "view.zoomOut"
  | "view.zoomReset"
  | "view.zenMode"
  | "settings.open"
  | "sidebar.toggle"
  | "editor.undo"
  | "editor.redo"
  | "editor.save";

export type ShortcutGroup =
  | "General"
  | "Tabs"
  | "Spaces"
  | "Terminal"
  | "Search"
  | "View"
  | "Editor";

export type KeyBinding = {
  key: string;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  meta?: boolean;
};

export type Shortcut = {
  id: ShortcutId;
  label: string;
  group: ShortcutGroup;
  defaultBindings: KeyBinding[];
  allowRepeat?: boolean;
};

export const SHORTCUTS: Shortcut[] = [
  {
    id: "commandPalette.open",
    label: "Open command palette",
    group: "General",
    defaultBindings: [{ [MOD_PROP]: true, key: "p" }],
  },
  {
    id: "commandPalette.content",
    label: "Find in files",
    group: "General",
    defaultBindings: [{ [MOD_PROP]: true, shift: true, key: "p" }],
  },
  {
    id: "settings.open",
    label: "Open settings",
    group: "General",
    defaultBindings: [{ [MOD_PROP]: true, key: "," }],
  },
  {
    id: "tab.new",
    label: "New tab",
    group: "Tabs",
    defaultBindings: [{ [MOD_PROP]: true, key: "t" }],
  },
  {
    id: "tab.newPreview",
    label: "New web preview",
    group: "Tabs",
    // Cmd/Ctrl+P now opens the command palette, so web preview moves here.
    defaultBindings: [{ [MOD_PROP]: true, shift: true, key: "o" }],
  },
  {
    id: "tab.close",
    label: "Close tab or pane",
    group: "Tabs",
    defaultBindings: [{ [MOD_PROP]: true, key: "w" }],
  },
  {
    id: "terminal.clear",
    label: "Clear terminal",
    group: "Terminal",
    // macOS Terminal's ⌘K (clear scrollback, keep the prompt). Default only on
    // macOS — on other platforms Ctrl+K is readline's kill-line, so we leave it
    // unbound and let users assign their own in settings.
    defaultBindings: IS_MAC ? [{ meta: true, key: "k" }] : [],
  },
  {
    id: "blocks.prev",
    label: "Previous command block",
    group: "Terminal",
    defaultBindings: [{ [MOD_PROP]: true, key: "ArrowUp" }],
    allowRepeat: true,
  },
  {
    id: "blocks.next",
    label: "Next command block",
    group: "Terminal",
    defaultBindings: [{ [MOD_PROP]: true, key: "ArrowDown" }],
    allowRepeat: true,
  },
  {
    id: "tab.next",
    label: "Next tab",
    group: "Tabs",
    defaultBindings: [{ ctrl: true, key: "Tab" }],
    allowRepeat: true,
  },
  {
    id: "tab.prev",
    label: "Previous tab",
    group: "Tabs",
    defaultBindings: [{ ctrl: true, shift: true, key: "Tab" }],
    allowRepeat: true,
  },
  {
    id: "tab.selectByIndex",
    label: "Jump to tab 1–9",
    group: "Tabs",
    defaultBindings: [{ [MOD_PROP]: true, key: "1" }],
  },
  {
    id: "space.next",
    label: "Next space",
    group: "Spaces",
    defaultBindings: [{ [MOD_PROP]: true, shift: true, key: "]" }],
  },
  {
    id: "space.prev",
    label: "Previous space",
    group: "Spaces",
    defaultBindings: [{ [MOD_PROP]: true, shift: true, key: "[" }],
  },
  {
    id: "space.overview",
    label: "Open spaces",
    group: "Spaces",
    defaultBindings: [{ [MOD_PROP]: true, shift: true, key: "s" }],
  },
  {
    id: "explorer.search",
    label: "Search files",
    group: "Search",
    defaultBindings: [{ [MOD_PROP]: true, shift: true, key: "f" }],
  },
  {
    id: "search.focus",
    label: "Find in tab",
    group: "Search",
    defaultBindings: [{ [MOD_PROP]: true, key: "f" }],
  },
  {
    id: "sidebar.toggle",
    label: "Toggle file explorer",
    group: "View",
    // Plain Mod+B toggles the sidebar everywhere EXCEPT a focused terminal,
    // where it's handed to the shell / Claude Code (its "run in background"
    // key). Mod+Shift+B always toggles, including from inside a terminal.
    defaultBindings: [
      { [MOD_PROP]: true, key: "b" },
      { [MOD_PROP]: true, shift: true, key: "b" },
    ],
  },
  {
    id: "explorer.focus",
    label: "Toggle file explorer focus",
    group: "View",
    defaultBindings: [{ [MOD_PROP]: true, shift: true, key: "e" }],
  },
  {
    id: "explorer.toggleHidden",
    label: "Toggle hidden files",
    group: "View",
    // Finder's toggle. The binding is on the physical Period key, so it holds
    // on layouts where Shift+. types something else.
    defaultBindings: [{ [MOD_PROP]: true, shift: true, key: "." }],
  },
  {
    id: "view.zoomIn",
    label: "Zoom in",
    group: "View",
    defaultBindings: [
      { [MOD_PROP]: true, key: "=" },
      { [MOD_PROP]: true, shift: true, key: "+" },
    ],
    allowRepeat: true,
  },
  {
    id: "view.zoomOut",
    label: "Zoom out",
    group: "View",
    defaultBindings: [
      { [MOD_PROP]: true, key: "-" },
      { [MOD_PROP]: true, shift: true, key: "_" },
    ],
    allowRepeat: true,
  },
  {
    id: "view.zoomReset",
    label: "Reset zoom",
    group: "View",
    defaultBindings: [{ [MOD_PROP]: true, key: "0" }],
  },
  {
    id: "view.zenMode",
    label: "Toggle zen mode",
    group: "View",
    defaultBindings: [{ [MOD_PROP]: true, shift: true, key: "'" }],
  },
  // Editor undo/redo: App.tsx registers handlers and useGlobalShortcuts
  // preventDefaults on match. Keep Mod+Y and add macOS Mod+Shift+Z (#941).
  // Still excluded from the customization UI in ShortcutsSection.
  {
    id: "editor.undo",
    label: "Undo",
    group: "Editor",
    defaultBindings: [{ [MOD_PROP]: true, key: "z" }],
  },
  {
    id: "editor.redo",
    label: "Redo",
    group: "Editor",
    defaultBindings: [
      { [MOD_PROP]: true, key: "y" },
      { [MOD_PROP]: true, shift: true, key: "z" },
    ],
  },
  // Mod+S at the app layer so WKWebView cannot steal "Save Page" (#969).
  // CodeMirror also binds Mod-s; the global handler preventDefaults first.
  {
    id: "editor.save",
    label: "Save file",
    group: "Editor",
    defaultBindings: [{ [MOD_PROP]: true, key: "s" }],
  },
];

export const SHORTCUT_GROUPS: ShortcutGroup[] = [
  "General",
  "Tabs",
  "Terminal",
  "View",
  "Search",
  "Editor",
];

/**
 * Matching logic: checks if a KeyboardEvent matches a KeyBinding.
 */
const CODE_TO_KEY: Record<string, string> = {
  Backslash: "\\",
  Slash: "/",
  BracketLeft: "[",
  BracketRight: "]",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  Space: " ",
};

// Option and Shift combinations rewrite e.key (macOS Option gives "«", "…",
// dead keys; Shift turns "." into ">"); the physical key survives in e.code.
function keyFromCode(code: string): string | null {
  if (code.startsWith("Key")) return code.slice(3).toLowerCase();
  if (code.startsWith("Digit")) return code.slice(5);
  return CODE_TO_KEY[code] ?? null;
}

export function matchBinding(
  e: KeyboardEvent,
  binding: KeyBinding,
  id?: ShortcutId
): boolean {
  const eventKey = e.key.toLowerCase();
  const bindingKey = binding.key.toLowerCase();

  // Special case for Jump to Tab 1-9
  if (id === "tab.selectByIndex") {
    if (!/^[1-9]$/.test(e.key)) return false;
  } else if (eventKey !== bindingKey) {
    if (!binding.alt && !binding.shift) return false;
    if (keyFromCode(e.code) !== bindingKey) return false;
  }

  return (
    !!e.ctrlKey === !!binding.ctrl &&
    !!e.shiftKey === !!binding.shift &&
    !!e.altKey === !!binding.alt &&
    !!e.metaKey === !!binding.meta
  );
}

/**
 * Display helpers
 */
export function getBindingTokens(binding?: KeyBinding): string[] {
  if (!binding) return [];
  const tokens: string[] = [];
  if (IS_MAC) {
    if (binding.ctrl) tokens.push("⌃");
    if (binding.alt) tokens.push("⌥");
    if (binding.shift) tokens.push("⇧");
    if (binding.meta) tokens.push("⌘");
  } else {
    if (binding.ctrl) tokens.push("Ctrl");
    if (binding.alt) tokens.push("Alt");
    if (binding.shift) tokens.push("Shift");
    if (binding.meta) tokens.push("Win");
  }

  let keyLabel = binding.key;
  if (keyLabel === " ") keyLabel = "Space";
  else if (keyLabel === "ArrowUp") keyLabel = "↑";
  else if (keyLabel === "ArrowDown") keyLabel = "↓";
  else if (keyLabel === "ArrowLeft") keyLabel = "←";
  else if (keyLabel === "ArrowRight") keyLabel = "→";
  else if (keyLabel.length === 1) keyLabel = keyLabel.toUpperCase();

  tokens.push(keyLabel);
  return tokens;
}

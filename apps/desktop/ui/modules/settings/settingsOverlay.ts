import { create } from "zustand";

const SETTINGS_TABS = [
  "general",
  "editor",
  "themes",
  "shortcuts",
  "models",
  "memory",
  "agents",
  "subagents",
  "plugins",
  "market",
  "mcp",
  "skills",
  "commands",
  "hooks",
  "archives",
  "about",
] as const;
export type SettingsTab = (typeof SETTINGS_TABS)[number];

export function resolveSettingsTab(value: unknown): SettingsTab | undefined {
  if (value === "ai" || value === "connections") return "models";
  if (value === "extensions") return "plugins";
  return SETTINGS_TABS.includes(value as SettingsTab)
    ? (value as SettingsTab)
    : undefined;
}

export const useSettingsOverlay = create<{
  open: boolean;
  tab: SettingsTab;
  show: (tab?: unknown) => void;
  close: () => void;
  select: (tab: SettingsTab) => void;
}>((set) => ({
  open: false,
  tab: "general",
  // 重复打开复用同一层；未指定分类时保留上次设置位置。
  show: (value) =>
    set((state) => ({
      open: true,
      tab: resolveSettingsTab(value) ?? state.tab,
    })),
  close: () => set({ open: false }),
  select: (tab) => set({ tab }),
}));

/** 覆盖层中仅保留设置和缩放快捷键，避免操作背后的任务及终端。 */
export function isSettingsShortcutAllowed(id: string): boolean {
  return [
    "settings.open",
    "view.zoomIn",
    "view.zoomOut",
    "view.zoomReset",
  ].includes(id);
}

import {
  useSettingsOverlay,
  type SettingsTab,
} from "@/modules/settings/settingsOverlay";
export type { SettingsTab };

export async function openSettingsWindow(tab?: SettingsTab): Promise<void> {
  useSettingsOverlay.getState().show(tab);
}

export async function returnToConversation(): Promise<void> {
  useSettingsOverlay.getState().close();
}

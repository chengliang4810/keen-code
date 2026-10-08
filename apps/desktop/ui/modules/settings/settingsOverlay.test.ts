import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  isSettingsShortcutAllowed,
  resolveSettingsTab,
  useSettingsOverlay,
} from "@/modules/settings/settingsOverlay";
import {
  openSettingsWindow,
  returnToConversation,
} from "@/modules/settings/openSettingsWindow";
import { invoke } from "@tauri-apps/api/core";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
beforeEach(() => {
  useSettingsOverlay.setState({ open: false, tab: "general" });
  vi.mocked(invoke).mockClear();
});
describe("settings overlay", () => {
  it("opens and returns in the same frontend without creating or closing a native window", async () => {
    await openSettingsWindow("models");
    expect(useSettingsOverlay.getState()).toMatchObject({
      open: true,
      tab: "models",
    });
    await returnToConversation();
    expect(useSettingsOverlay.getState().open).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });
  it("reuses one overlay and remembers the selected category when reopened", async () => {
    await openSettingsWindow();
    useSettingsOverlay.getState().select("archives");
    await openSettingsWindow("themes");
    expect(useSettingsOverlay.getState().tab).toBe("themes");
    await returnToConversation();
    await openSettingsWindow();
    expect(useSettingsOverlay.getState()).toMatchObject({
      open: true,
      tab: "themes",
    });
  });
  it("accepts legacy category names and ignores malformed native event payloads", () => {
    expect(resolveSettingsTab("ai")).toBe("models");
    expect(resolveSettingsTab("connections")).toBe("models");
    expect(resolveSettingsTab("agents")).toBe("agents");
    expect(resolveSettingsTab("subagents")).toBe("subagents");
    expect(resolveSettingsTab("extensions")).toBe("plugins");
    expect(resolveSettingsTab("market")).toBe("market");
    for (const payload of [undefined, null, {}, ["models"], "invalid"]) {
      expect(resolveSettingsTab(payload)).toBeUndefined();
      useSettingsOverlay.getState().show(payload);
      expect(useSettingsOverlay.getState().tab).toBe("general");
    }
  });
  it("blocks shortcuts that could alter the hidden task while retaining settings and zoom", () => {
    for (const id of [
      "tab.close",
      "tab.new",
      "editor.save",
      "terminal.clear",
      "space.next",
      "ai.toggle",
      "sidebar.toggle",
    ])
      expect(isSettingsShortcutAllowed(id)).toBe(false);
    for (const id of [
      "settings.open",
      "view.zoomIn",
      "view.zoomOut",
      "view.zoomReset",
    ])
      expect(isSettingsShortcutAllowed(id)).toBe(true);
  });
});

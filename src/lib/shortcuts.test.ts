import { describe, expect, it } from "vitest";
import {
  SHORTCUTS,
  shortcutsForPlatform,
} from "./shortcuts";

describe("shortcuts catalog", () => {
  it("has stable unique ids", () => {
    const ids = SHORTCUTS.map((s) => s.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("every row has mac and win bindings", () => {
    for (const s of SHORTCUTS) {
      expect(s.mac.trim().length).toBeGreaterThan(0);
      expect(s.win.trim().length).toBeGreaterThan(0);
      expect(s.labelKey.startsWith("shortcuts.")).toBe(true);
    }
  });

  it("picks platform-specific keys", () => {
    const mac = shortcutsForPlatform("mac");
    const win = shortcutsForPlatform("win");
    const searchMac = mac.find((s) => s.id === "search");
    const searchWin = win.find((s) => s.id === "search");
    expect(searchMac?.keys).toContain("⌘");
    expect(searchWin?.keys.toLowerCase()).toContain("ctrl");
  });
});

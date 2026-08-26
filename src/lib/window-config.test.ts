/**
 * Window chrome: mac Overlay traffic lights; Windows frameless + self-drawn controls.
 */
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const TAURI_DIR = resolve(__dirname, "../../src-tauri");
const CONF_PATH = resolve(TAURI_DIR, "tauri.conf.json");
const MAC_PATH = resolve(TAURI_DIR, "tauri.macos.conf.json");
const WIN_PATH = resolve(TAURI_DIR, "tauri.windows.conf.json");

describe("window chrome", () => {
  it("ships platform-specific window configs", () => {
    expect(existsSync(CONF_PATH)).toBe(true);
    expect(existsSync(MAC_PATH)).toBe(true);
    expect(existsSync(WIN_PATH)).toBe(true);
  });

  it("keeps the main window usable at compact desktop widths", () => {
    for (const path of [CONF_PATH, MAC_PATH, WIN_PATH]) {
      const conf = JSON.parse(readFileSync(path, "utf8")) as {
        app: { windows: Array<{ minWidth?: number }> };
      };
      expect(conf.app.windows[0]!.minWidth).toBe(680);
    }
  });

  it("mac uses Overlay traffic lights without system title text", () => {
    const conf = JSON.parse(readFileSync(MAC_PATH, "utf8")) as {
      app: {
        macOSPrivateApi?: boolean;
        windows: Array<{
          decorations?: boolean;
          titleBarStyle?: string;
          hiddenTitle?: boolean;
          trafficLightPosition?: { x: number; y: number };
          transparent?: boolean;
        }>;
      };
    };
    const main = conf.app.windows[0]!;
    expect(main.titleBarStyle).toBe("Overlay");
    expect(main.hiddenTitle).toBe(true);
    expect(main.trafficLightPosition).toBeTruthy();
    expect(main.transparent).toBe(true);
    expect(main.decorations).toBe(true);
    expect(conf.app.macOSPrivateApi).toBe(true);
  });

  it("windows is frameless for self-drawn controls", () => {
    const conf = JSON.parse(readFileSync(WIN_PATH, "utf8")) as {
      app: {
        windows: Array<{
          decorations?: boolean;
          transparent?: boolean;
        }>;
      };
    };
    const main = conf.app.windows[0]!;
    expect(main.decorations).toBe(false);
    expect(main.transparent).toBe(false);
  });

  it("base product identity is KeenCode", () => {
    const conf = JSON.parse(readFileSync(CONF_PATH, "utf8")) as {
      productName?: string;
      app: { windows: Array<{ title?: string }> };
    };
    expect(conf.productName).toBe("KeenCode");
    expect(conf.app.windows[0]!.title).toBe("KeenCode");
  });

  it("ships a restrictive production content security policy", () => {
    const conf = JSON.parse(readFileSync(CONF_PATH, "utf8")) as {
      app: { security?: { csp?: string | null } };
    };
    const csp = conf.app.security?.csp;
    expect(typeof csp).toBe("string");
    expect(csp).toContain("default-src 'self'");
    expect(csp).toContain("object-src 'none'");
  });

  it("does not retain an unused native vibrancy dependency", () => {
    const cargo = readFileSync(resolve(TAURI_DIR, "Cargo.toml"), "utf8");
    expect(cargo).not.toMatch(/window-vibrancy/);
  });
});

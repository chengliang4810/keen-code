import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { readCssSource } from "../test-utils/readCssSource";

const tokens = readFileSync(new URL("../styles/tokens.css", import.meta.url), "utf8");
const appFoundation = readFileSync(
  new URL("../styles/app-foundation.css", import.meta.url),
  "utf8",
);
const governance = readFileSync(
  new URL("../styles/ui-governance.css", import.meta.url),
  "utf8",
);
const skins = readFileSync(new URL("../styles/skins.css", import.meta.url), "utf8");
const tailwind = readFileSync(new URL("../styles/tailwind.css", import.meta.url), "utf8");
const settingsShell = readFileSync(
  new URL("../styles/settings-shell.css", import.meta.url),
  "utf8",
);
const resourceStyles = readFileSync(
  new URL("../styles/app-resource.css", import.meta.url),
  "utf8",
);
const terminalPanel = readFileSync(
  new URL("../components/TerminalPanel.tsx", import.meta.url),
  "utf8",
);
const main = readFileSync(new URL("../main.tsx", import.meta.url), "utf8");
const appStyles = readFileSync(new URL("../styles/app.css", import.meta.url), "utf8");
const expandedAppStyles = readCssSource(new URL("../styles/app.css", import.meta.url));
const appicaStyles = readFileSync(
  new URL("../../node_modules/@appica/ui-react/styles.css", import.meta.url),
  "utf8",
);
const finalAppStyles = `${appicaStyles}\n${expandedAppStyles}`;

const APPICA_ROLE_TOKENS = [
  "foreground",
  "foreground-subtle",
  "foreground-subtlest",
  "foreground-muted",
  "foreground-strong",
  "foreground-emphasis",
  "foreground-intense",
  "foreground-inverse",
  "background",
  "background-subtle",
  "background-muted",
  "background-strong",
  "background-inverse",
  "border",
  "border-muted",
  "border-strong",
  "border-emphasis",
  "border-intense",
  "border-inverse",
  "border-overlay",
  "primary",
  "primary-subtle",
  "primary-soft",
  "primary-muted",
  "primary-strong",
  "primary-foreground",
  "secondary",
  "secondary-subtle",
  "secondary-soft",
  "secondary-muted",
  "secondary-strong",
  "secondary-emphasis",
  "secondary-intense",
  "secondary-foreground",
  "error",
  "error-subtle",
  "error-soft",
  "error-muted",
  "error-strong",
  "error-emphasis",
  "error-intense",
  "error-foreground",
  "success",
  "success-subtle",
  "success-soft",
  "success-muted",
  "success-strong",
  "success-emphasis",
  "success-intense",
  "success-foreground",
  "warning",
  "warning-subtle",
  "warning-soft",
  "warning-muted",
  "warning-strong",
  "warning-emphasis",
  "warning-intense",
  "warning-foreground",
  "info",
  "info-subtle",
  "info-soft",
  "info-muted",
  "info-strong",
  "info-emphasis",
  "info-intense",
  "info-foreground",
  "focus-ring",
  "focus-ring-input",
  "focus-ring-primary",
  "focus-ring-secondary",
  "focus-ring-error",
  "focus-ring-success",
  "focus-ring-warning",
  "focus-ring-info",
  "focus-ring-light",
  "selection-color",
  "tooltip",
  "tooltip-foreground",
];

function declaration(source: string, name: string): string {
  const match = source.match(new RegExp(`^\\s*--${name}:\\s*([^;]+);`, "m"));
  expect(match, `缺少 --${name} 声明`).toBeTruthy();
  return match?.[1] ?? "";
}

function scopeBody(source: string, selector: string): string {
  const start = source.indexOf(selector);
  expect(start, `缺少选择器 ${selector}`).toBeGreaterThanOrEqual(0);
  const open = source.indexOf("{", start);
  const close = source.indexOf("}", open);
  expect(close).toBeGreaterThan(open);
  return source.slice(open + 1, close);
}

function lastScopeBody(source: string, selector: string): string {
  const start = source.lastIndexOf(selector);
  expect(start, `缺少选择器 ${selector}`).toBeGreaterThanOrEqual(0);
  const open = source.indexOf("{", start);
  const close = source.indexOf("}", open);
  expect(close).toBeGreaterThan(open);
  return source.slice(open + 1, close);
}

describe("Appica theme token bridge", () => {
  it("keeps Appica role declarations in one post-Appica authority", () => {
    const light = scopeBody(governance, '[data-theme="light"] {');
    const dark = scopeBody(
      governance,
      ':root:not([data-theme="light"]),\n[data-theme="dark"] {',
    );

    for (const role of APPICA_ROLE_TOKENS) {
      expect(tokens, `tokens.css 不应声明 --${role}`).not.toMatch(
        new RegExp(`^\\s*--${role}:`, "m"),
      );
      expect(light, `浅色桥接缺少 --${role}`).toMatch(
        new RegExp(`^\\s*--${role}:`, "m"),
      );
      expect(dark, `深色桥接缺少 --${role}`).toMatch(
        new RegExp(`^\\s*--${role}:`, "m"),
      );
    }
  });

  it("loads the bridge after Appica and keeps ZCode Zai surfaces", () => {
    expect(main.indexOf('import "./styles/tailwind.css";')).toBeLessThan(
      main.indexOf('import "./styles/app.css";'),
    );
    expect(appStyles.indexOf('@import "./ui-governance.css";')).toBeGreaterThan(
      appStyles.indexOf('@import "./app-features.css";'),
    );

    const light = scopeBody(governance, '[data-theme="light"] {');
    const dark = scopeBody(
      governance,
      ':root:not([data-theme="light"]),\n[data-theme="dark"] {',
    );
    expect(declaration(light, "background")).toBe("#f8f8f8");
    expect(declaration(light, "foreground")).toBe("#282828");
    expect(declaration(light, "primary")).toBe("#000000");
    expect(declaration(dark, "background")).toBe("#161616");
    expect(declaration(dark, "background-strong")).toBe("#363636");
    expect(declaration(dark, "primary")).toBe("#ffffff");
  });

  it("leaves accent and focus ownership to product skins", () => {
    expect(governance).not.toMatch(/^\s*--accent:/m);
    expect(governance).not.toMatch(/^\s*--border-focus:/m);
    expect(tokens).toMatch(/^\s*--accent:/m);
    expect(tokens).toMatch(/^\s*--border-focus:/m);
    expect(skins).toMatch(/\[data-theme="dark"\]\[data-skin="rose"\][\s\S]*--accent:/);
    expect(skins).toMatch(/\[data-theme="dark"\]\[data-skin="rose"\][\s\S]*--border-focus:/);
  });

  it("keeps terminal and resource accents aligned with ZCode light/dark values", () => {
    const light = scopeBody(tokens, ":root {");
    const dark = lastScopeBody(tokens, '[data-theme="dark"] {');

    expect(declaration(light, "terminal-bg")).toBe("#f8f8f8");
    expect(declaration(light, "terminal-fg")).toBe("#282828");
    expect(declaration(light, "terminal-cursor")).toBe("#0d0d0d");
    expect(declaration(light, "terminal-cursor-accent")).toBe("#f8f8f8");
    expect(declaration(light, "terminal-selection")).toBe(
      "rgba(11, 127, 255, 0.22)",
    );
    expect(declaration(light, "terminal-selection-inactive")).toBe(
      "rgba(13, 13, 13, 0.1)",
    );

    expect(declaration(dark, "terminal-bg")).toBe("#161616");
    expect(declaration(dark, "terminal-fg")).toBe("#d4d4d4");
    expect(declaration(dark, "terminal-cursor")).toBe("#f8f8f8");
    expect(declaration(dark, "terminal-cursor-accent")).toBe("#161616");
    expect(declaration(dark, "terminal-selection")).toBe(
      "rgba(64, 153, 255, 0.28)",
    );
    expect(declaration(dark, "terminal-selection-inactive")).toBe(
      "rgba(255, 255, 255, 0.1)",
    );

    const lightAccents = {
      added: declaration(light, "change-added"),
      deleted: declaration(light, "change-deleted"),
      renamed: declaration(light, "change-renamed"),
      git: declaration(light, "kind-git"),
      image: declaration(light, "kind-img"),
      directory: declaration(light, "dir-accent"),
    };
    expect(lightAccents).toEqual({
      added: "#1e8a3e",
      deleted: "#e03131",
      renamed: "#0b7fff",
      git: "#e07b00",
      image: "#7453b0",
      directory: "#1a70b8",
    });

    const darkAccents = {
      added: declaration(dark, "change-added"),
      deleted: declaration(dark, "change-deleted"),
      renamed: declaration(dark, "change-renamed"),
      git: declaration(dark, "kind-git"),
      image: declaration(dark, "kind-img"),
      directory: declaration(dark, "dir-accent"),
    };
    expect(darkAccents).toEqual({
      added: "#46bf72",
      deleted: "#ff5c5c",
      renamed: "#4099ff",
      git: "#ff8a30",
      image: "#bda5e6",
      directory: "#8fc5ef",
    });
  });

  it("keeps terminal rendering CSS-backed and resource selectors semantic", () => {
    expect(terminalPanel).toContain('theme: readTerminalTheme(),');
    expect(terminalPanel).toContain('background: read("--terminal-bg", "--bg-main")');
    expect(terminalPanel).toContain(
      'selectionBackground: read("--terminal-selection", "--accent-muted")',
    );
    expect(terminalPanel).not.toMatch(/theme:\s*\{[\s\S]*#[0-9a-f]/i);
    expect(resourceStyles).toContain("background: var(--terminal-bg);");
    expect(resourceStyles).toContain("color: var(--change-added);");
    expect(resourceStyles).toContain("color: var(--change-renamed);");
    expect(resourceStyles).toContain("color: var(--dir-accent);");
  });

  it("keeps text selection ownership out of the global foundation stylesheet", () => {
    expect(appFoundation).not.toMatch(/::selection\s*\{/);
    expect(tokens).toMatch(/^\s*--terminal-selection:/m);
    expect(tokens).toMatch(/^\s*--terminal-selection-inactive:/m);
  });

  it("neutralizes Appica global selection and focus rules in the final import chain", () => {
    const appicaSelection = appicaStyles.indexOf("::selection");
    const selectionReset = finalAppStyles.lastIndexOf("::selection");
    expect(appicaSelection).toBeGreaterThanOrEqual(0);
    expect(selectionReset).toBeGreaterThan(appicaSelection);
    expect(finalAppStyles).toMatch(
      /::selection\s*\{\s*background-color:\s*revert;\s*color:\s*revert;\s*\}/,
    );

    const appicaFocus = appicaStyles.indexOf("focus-visible:outline-3");
    const focusReset = finalAppStyles.lastIndexOf(":where(*):focus-visible");
    expect(appicaFocus).toBeGreaterThanOrEqual(0);
    expect(focusReset).toBeGreaterThan(appicaFocus);
    expect(governance).not.toMatch(
      /outline:\s*2px solid var\(--border-focus\)/,
    );
  });

  it("maps hover and focus utilities to the product interaction tokens", () => {
    expect(tailwind).toMatch(/^\s*--color-hover:\s*var\(--bg-hover\);/m);
    expect(tailwind).toMatch(/^\s*--color-ring:\s*var\(--focus-ring\);/m);
    expect(tailwind).toMatch(
      /^\s*--color-input-border-focused:\s*var\(--border-strong\);/m,
    );
    expect(tailwind).not.toMatch(/^\s*--color-ring:\s*var\(--border-focus\);/m);
  });

  it("keeps Appica foreground roles on the theme bridge", () => {
    expect(tailwind).toMatch(
      /^\s*--color-primary-foreground:\s*var\(--primary-foreground\);/m,
    );
    expect(tailwind).toMatch(
      /^\s*--color-secondary-foreground:\s*var\(--secondary-foreground\);/m,
    );
    expect(tailwind).toMatch(
      /^\s*--color-destructive-foreground:\s*var\(--error-foreground\);/m,
    );
    expect(tailwind).not.toMatch(
      /^\s*--color-destructive-foreground:\s*#fff;/im,
    );
  });

  it("keeps Settings portal surfaces on semantic ZCode roles", () => {
    expect(declaration(tokens, "bg-overlay")).toBe("rgba(0, 0, 0, 0.6)");
    expect(declaration(tokens, "border-popover")).toBe("var(--border-subtle)");
    const dark = lastScopeBody(tokens, '[data-theme="dark"] {');
    expect(declaration(dark, "bg-overlay")).toBe("rgba(0, 0, 0, 0.6)");
    expect(declaration(dark, "border-popover")).toBe("var(--border-subtle)");
    expect(settingsShell).toContain('background: var(--bg-overlay);');
    expect(settingsShell).toContain('border: 1px solid var(--border-popover);');
    expect(settingsShell).toContain('border: 1px solid var(--border-subtle);');
    expect(settingsShell).not.toMatch(/(?:background|border):[^;]*(?:rgba?\(|#[0-9a-f])/i);
  });

  it("matches ZCode user bubble opacity in light mode without changing dark mode", () => {
    expect(tokens).toContain(
      "  --dsw-specific-bubble: color-mix(in oklab, #0d0d0d 3%, transparent);",
    );
    expect(tokens).toContain(
      "  --dsw-specific-bubble: rgba(255, 255, 255, 0.05);",
    );
  });
});

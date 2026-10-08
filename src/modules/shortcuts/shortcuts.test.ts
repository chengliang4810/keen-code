import { describe, expect, it } from "vitest";
import { MOD_PROP } from "@/lib/platform";
import {
  getBindingTokens,
  type KeyBinding,
  matchBinding,
  SHORTCUTS,
} from "./shortcuts";

// These tests run in the vitest node environment, where the Tauri OS plugin is
// unavailable so `IS_MAC` resolves to false. That makes the non-mac token
// branch deterministic across host platforms.

function event(over: Partial<KeyboardEvent>): KeyboardEvent {
  return {
    key: "",
    code: "",
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    metaKey: false,
    ...over,
  } as KeyboardEvent;
}

describe("getBindingTokens", () => {
  it("returns nothing for an undefined binding", () => {
    expect(getBindingTokens(undefined)).toEqual([]);
  });

  it("lists modifiers in order, then the key", () => {
    const binding: KeyBinding = { key: "k", ctrl: true, shift: true };
    expect(getBindingTokens(binding)).toEqual(["Ctrl", "Shift", "K"]);
  });

  it("labels space and arrow keys", () => {
    expect(getBindingTokens({ key: " ", meta: true })).toEqual([
      "Win",
      "Space",
    ]);
    expect(getBindingTokens({ key: "ArrowUp", alt: true })).toEqual([
      "Alt",
      "↑",
    ]);
  });

  it("uppercases a single-character key", () => {
    expect(getBindingTokens({ key: "c" })).toEqual(["C"]);
  });
});

describe("matchBinding", () => {
  it("matches when key and all modifiers agree", () => {
    expect(
      matchBinding(event({ key: "c", ctrlKey: true }), {
        key: "c",
        ctrl: true,
      }),
    ).toBe(true);
  });

  it("matches the key case-insensitively", () => {
    expect(
      matchBinding(event({ key: "C", ctrlKey: true }), {
        key: "c",
        ctrl: true,
      }),
    ).toBe(true);
  });

  it("fails when a required modifier is missing", () => {
    expect(matchBinding(event({ key: "c" }), { key: "c", ctrl: true })).toBe(
      false,
    );
  });

  it("fails when an extra modifier is pressed", () => {
    expect(
      matchBinding(event({ key: "c", ctrlKey: true, shiftKey: true }), {
        key: "c",
        ctrl: true,
      }),
    ).toBe(false);
  });

  it("falls back to the physical code for alt combinations", () => {
    // Alt often rewrites e.key (here to "ç"); the binding still matches via e.code.
    expect(
      matchBinding(event({ key: "ç", code: "KeyC", altKey: true }), {
        key: "c",
        alt: true,
      }),
    ).toBe(true);
    expect(
      matchBinding(event({ key: "ç", code: "KeyD", altKey: true }), {
        key: "c",
        alt: true,
      }),
    ).toBe(false);
  });

  it("falls back to the physical code for shift combinations", () => {
    // Shift turns Period into ">"; ⌘⇧. still matches the "." binding.
    expect(
      matchBinding(
        event({ key: ">", code: "Period", shiftKey: true, metaKey: true }),
        { key: ".", shift: true, meta: true },
      ),
    ).toBe(true);
    expect(
      matchBinding(
        event({ key: ">", code: "Comma", shiftKey: true, metaKey: true }),
        { key: ".", shift: true, meta: true },
      ),
    ).toBe(false);
  });

  it("does not fall back to the physical code without alt or shift", () => {
    expect(
      matchBinding(event({ key: "ç", code: "KeyC", metaKey: true }), {
        key: "c",
        meta: true,
      }),
    ).toBe(false);
  });

  it("only accepts digit keys for the jump-to-tab shortcut", () => {
    expect(
      matchBinding(event({ key: "3" }), { key: "1" }, "tab.selectByIndex"),
    ).toBe(true);
    expect(
      matchBinding(event({ key: "x" }), { key: "1" }, "tab.selectByIndex"),
    ).toBe(false);
  });
});

describe("SHORTCUTS registry", () => {
  it.each<[string, Partial<KeyboardEvent>]>([
    ["toggle AI", { key: "i", ctrlKey: true }],
    ["toggle AI window", { key: "i", ctrlKey: true, shiftKey: true }],
    ["ask AI", { key: "j", ctrlKey: true }],
    ["agent attention", { key: "a", ctrlKey: true, shiftKey: true }],
    ["AI completion", { key: "\\", code: "Backslash", altKey: true }],
    ["code completion", { key: " ", code: "Space", ctrlKey: true }],
    ["split right", { key: "d", ctrlKey: true }],
    ["split down", { key: "d", ctrlKey: true, shiftKey: true }],
    ["focus next pane", { key: "]", ctrlKey: true }],
    ["focus previous pane", { key: "[", ctrlKey: true }],
    ["swap left", { key: "ArrowLeft", ctrlKey: true, altKey: true }],
    ["swap right", { key: "ArrowRight", ctrlKey: true, altKey: true }],
    ["swap up", { key: "ArrowUp", ctrlKey: true, altKey: true }],
    ["swap down", { key: "ArrowDown", ctrlKey: true, altKey: true }],
    ["source panel", { key: "g", ctrlKey: true }],
    ["shell input", { key: "u", ctrlKey: true }],
    ["block terminal", { key: "t", ctrlKey: true, shiftKey: true }],
    ["private terminal", { key: "r", ctrlKey: true }],
    ["new editor", { key: "e", ctrlKey: true }],
  ])("leaves the removed %s binding to the focused view", (_label, pressed) => {
    const keyboardEvent = event(pressed);
    expect(
      SHORTCUTS.some((shortcut) =>
        shortcut.defaultBindings.some((binding) =>
          matchBinding(keyboardEvent, binding, shortcut.id),
        ),
      ),
    ).toBe(false);
  });

  it("binds Mod+S to editor.save", () => {
    const save = SHORTCUTS.find((s) => s.id === "editor.save");
    expect(save).toBeDefined();
    expect(save!.defaultBindings).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ key: "s", [MOD_PROP]: true }),
      ]),
    );
  });
});

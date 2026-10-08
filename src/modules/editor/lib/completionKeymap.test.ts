import { describe, expect, it } from "vitest";
import { startCompletion } from "@codemirror/autocomplete";
import { EditorState } from "@codemirror/state";
import { keymap } from "@codemirror/view";
import { editorCompletionExtension } from "@/modules/editor/lib/completionKeymap";

describe("editor completion keymap", () => {
  const bindings = () =>
    EditorState.create({ extensions: editorCompletionExtension })
      .facet(keymap)
      .flat();

  it("disables manual completion keys including the library's implicit bindings", () => {
    expect(bindings().some((binding) => binding.run === startCompletion)).toBe(
      false,
    );
  });

  it("retains navigation, dismissal and acceptance for automatic completions", () => {
    expect(bindings().map((binding) => binding.key)).toEqual(
      expect.arrayContaining([
        "ArrowDown",
        "ArrowUp",
        "PageDown",
        "PageUp",
        "Escape",
        "Enter",
        "Tab",
      ]),
    );
  });
});

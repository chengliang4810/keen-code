import { ensureSyntaxTree } from "@codemirror/language";
import { EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";
import { LANGUAGES } from "@/modules/editor/lib/languageDefinitions";
import { markdownCodeLanguages } from "@/modules/editor/lib/markdownExtras";

describe("lazy editor language definitions", () => {
  it.each(LANGUAGES)("loads the complete $name mode", async (definition) => {
    const extension = await definition.loader();
    const state = EditorState.create({
      doc: "value",
      extensions: [extension],
    });
    expect(ensureSyntaxTree(state, state.doc.length, 100)).not.toBeNull();
  });

  it("preserves GFM and typed fenced code through the shared lazy registry", async () => {
    const typescript = markdownCodeLanguages().find((language) =>
      language.alias.includes("ts"),
    );
    expect(typescript).toBeDefined();
    await typescript?.load();
    const markdown = LANGUAGES.find(
      (definition) => definition.name === "Markdown",
    );
    if (!markdown) throw new Error("Markdown definition missing");
    const state = EditorState.create({
      doc: [
        "| a | b |",
        "| - | - |",
        "| 1 | 2 |",
        "",
        "- [x] done",
        "",
        "~~gone~~",
        "",
        "```ts",
        "const value: number = 1;",
        "```",
      ].join("\n"),
      extensions: [await markdown.loader()],
    });
    const tree = ensureSyntaxTree(state, state.doc.length, 100);
    expect(tree?.toString()).toContain("Table");
    expect(tree?.toString()).toContain("Task");
    expect(tree?.toString()).toContain("Strikethrough");
    expect(
      tree?.resolveInner(state.doc.toString().indexOf("number") + 1).name,
    ).toBe("TypeName");
  });
});

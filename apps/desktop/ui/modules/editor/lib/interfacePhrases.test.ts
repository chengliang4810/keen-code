import { history, undoDepth } from "@codemirror/commands";
import { Compartment, EditorState } from "@codemirror/state";
import { afterEach, expect, it } from "vitest";
import { applyLanguagePreference, t } from "@/modules/i18n/state";
import { editorInterfacePhrases } from "./interfacePhrases";

afterEach(async () => {
  await applyLanguagePreference("en-US");
});

it("reconfigures Chinese editor labels without losing document, selection or undo history", async () => {
  await applyLanguagePreference("en-US");
  const phrases = new Compartment();
  let state = EditorState.create({
    doc: "中文 buffer",
    extensions: [history(), phrases.of(editorInterfacePhrases(t))],
  });
  state = state.update({
    changes: { from: state.doc.length, insert: " draft" },
    selection: { anchor: 4 },
  }).state;
  const previousDoc = state.doc.toString();
  const previousUndo = undoDepth(state);
  await applyLanguagePreference("zh-CN");
  state = state.update({
    effects: phrases.reconfigure(editorInterfacePhrases(t)),
  }).state;
  expect(state.phrase("Find")).toBe("查找");
  expect(state.phrase("replaced $ matches", 2)).toBe("已替换 2 处匹配");
  expect(state.doc.toString()).toBe(previousDoc);
  expect(state.selection.main.anchor).toBe(4);
  expect(undoDepth(state)).toBe(previousUndo);
  await applyLanguagePreference("en-US");
  state = state.update({
    effects: phrases.reconfigure(editorInterfacePhrases(t)),
  }).state;
  expect(state.phrase("Find")).toBe("Find");
});

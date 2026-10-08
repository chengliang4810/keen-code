import {
  acceptCompletion,
  autocompletion,
  completionKeymap,
  startCompletion,
} from "@codemirror/autocomplete";
import { type Extension, Prec } from "@codemirror/state";
import { keymap } from "@codemirror/view";

// 禁用库内置的手动补全快捷键，保留自动补全列表的导航与接受操作。
export const editorCompletionExtension: Extension = [
  autocompletion({ defaultKeymap: false }),
  Prec.highest(
    keymap.of([
      ...completionKeymap.filter((binding) => binding.run !== startCompletion),
      { key: "Tab", run: acceptCompletion },
    ]),
  ),
];

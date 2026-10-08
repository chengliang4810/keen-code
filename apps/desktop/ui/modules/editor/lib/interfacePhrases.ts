import type { MessageParams } from "@/modules/i18n/locale";
import { EditorState, type Extension } from "@codemirror/state";

const PHRASES = [
  "Find",
  "Replace",
  "next",
  "previous",
  "all",
  "match case",
  "regexp",
  "by word",
  "replace",
  "replace all",
  "close",
  "Go to line",
  "go",
  "current match",
  "on line",
  "replaced match on line $",
  "replaced $ matches",
  "Folded lines",
  "Unfolded lines",
  "to",
  "folded code",
  "unfold",
  "Fold line",
  "Unfold line",
  "$ unchanged lines",
  "Revert this chunk",
  "Accept",
  "Reject",
  "Control character",
] as const;

/** CodeMirror 使用 $ 插入参数；这里只翻译标题，不改变编辑器状态或其占位符。 */
export function editorInterfacePhrases(
  tr: (message: string, params?: MessageParams) => string,
): Extension {
  return EditorState.phrases.of(
    Object.fromEntries(PHRASES.map((message) => [message, tr(message)])),
  );
}

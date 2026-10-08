import { usePreferencesStore } from "@/modules/settings/preferences";
import { useTranslation } from "@/modules/i18n";
import { resolveEditorThemeId, useTheme } from "@/modules/theme";
import type { Extension } from "@codemirror/state";
import { useMemo } from "react";
import { EDITOR_THEME_EXT } from "./themes";
import { editorInterfacePhrases } from "./interfacePhrases";

/** Resolves the active CodeMirror theme extension, honoring the "auto" pairing. */
export function useEditorThemeExt(): Extension {
  const tr = useTranslation();
  const pref = usePreferencesStore((s) => s.editorTheme);
  const { themeId, customThemes, resolvedMode } = useTheme();
  return useMemo(() => {
    const id = resolveEditorThemeId(pref, themeId, customThemes, resolvedMode);
    // @uiw 只重配置扩展，保留当前文档、选择和撤销历史。
    return [
      EDITOR_THEME_EXT[id] ?? EDITOR_THEME_EXT.atomone,
      editorInterfacePhrases(tr),
    ];
  }, [pref, themeId, customThemes, resolvedMode, tr]);
}

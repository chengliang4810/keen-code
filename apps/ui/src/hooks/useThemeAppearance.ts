import { useEffect, useMemo, useState } from "react";
import {
  applyNativeWindowTheme,
  applyThemePreference,
  applyThemeToDocument,
  getSystemTheme,
  loadThemePreference,
  resolveTheme,
  saveThemePreference,
  subscribeSystemTheme,
  type ThemePreference,
} from "@/lib/theme";
import {
  applySkinToDocument,
  loadSkin,
  saveSkin,
  skinPreferredTheme,
  type ThemeSkinId,
} from "@/lib/themeSkin";
import {
  applyUiFontSizeToDocument,
  loadUiFontSize,
  saveUiFontSize,
} from "@/lib/uiFontSize";

export function useThemeAppearance() {
  const [themePreference, setThemePreference] = useState<ThemePreference>(() =>
    loadThemePreference(localStorage),
  );
  const [systemTheme, setSystemTheme] = useState(() => getSystemTheme());
  const theme = useMemo(
    () => resolveTheme(themePreference, systemTheme),
    [themePreference, systemTheme],
  );
  const [skin, setSkin] = useState<ThemeSkinId>(() => loadSkin(localStorage));
  const [uiFontSize, setUiFontSize] = useState(() =>
    loadUiFontSize(localStorage),
  );

  useEffect(() => {
    applyThemeToDocument(theme);
    void applyNativeWindowTheme(themePreference === "system" ? null : theme);
  }, [theme, themePreference]);

  useEffect(() => {
    applyUiFontSizeToDocument(uiFontSize);
  }, [uiFontSize]);

  useEffect(() => {
    if (themePreference !== "system") return;
    let cancelled = false;
    void applyNativeWindowTheme(null).then(() => {
      if (cancelled) return;
      const next = getSystemTheme();
      setSystemTheme(next);
      applyThemeToDocument(next);
    });
    const unsubscribe = subscribeSystemTheme((next) => {
      setSystemTheme(next);
      applyThemeToDocument(next);
      void applyNativeWindowTheme(null);
    });
    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, [themePreference]);

  useEffect(() => applySkinToDocument(skin), [skin]);

  const applyThemeChoice = (next: ThemePreference) => {
    saveThemePreference(localStorage, next);
    setThemePreference(next);
    void applyThemePreference(next, {
      onResolved: (resolved, system) => {
        setSystemTheme(next === "system" ? resolved : system);
      },
    });
  };

  const applySkinChoice = (next: ThemeSkinId) => {
    saveSkin(localStorage, next);
    applySkinToDocument(next);
    setSkin(next);
    const preferred = skinPreferredTheme(next);
    if (preferred && preferred !== theme) applyThemeChoice(preferred);
  };

  const applyUiFontSizeChoice = (next: number) => {
    saveUiFontSize(localStorage, next);
    applyUiFontSizeToDocument(next);
    setUiFontSize(next);
  };

  return {
    themePreference,
    skin,
    uiFontSize,
    applyThemeChoice,
    applySkinChoice,
    applyUiFontSizeChoice,
  };
}

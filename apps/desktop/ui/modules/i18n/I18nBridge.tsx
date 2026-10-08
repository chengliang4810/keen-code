import { usePreferencesStore } from "@/modules/settings/preferences";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect } from "react";
import { useLocale } from "./index";
import { applyLanguagePreference, refreshSystemLocale, t } from "./state";

export function I18nBridge({
  settingsWindow = false,
}: {
  settingsWindow?: boolean;
}) {
  const language = usePreferencesStore((s) => s.uiLanguage);
  const hydrated = usePreferencesStore((s) => s.hydrated);
  const locale = useLocale();

  useEffect(() => {
    if (hydrated)
      void applyLanguagePreference(language).catch((error) =>
        console.error("language loading failed", error),
      );
  }, [language, hydrated]);

  useEffect(() => {
    const refresh = () =>
      void refreshSystemLocale().catch((error) =>
        console.error("language loading failed", error),
      );
    window.addEventListener("languagechange", refresh);
    return () => window.removeEventListener("languagechange", refresh);
  }, []);

  useEffect(() => {
    document.documentElement.lang = locale;
    // 原生菜单仅由主窗口更新，避免不同 WebView 的初始化顺序覆盖设置。
    if (!settingsWindow) {
      void invoke("ui_set_locale", { locale }).catch((error) =>
        console.error("menu localization failed", error),
      );
    } else {
      void getCurrentWindow()
        .setTitle(t("Settings"))
        .catch((error) =>
          console.error("settings title localization failed", error),
        );
    }
  }, [locale, settingsWindow]);

  return null;
}

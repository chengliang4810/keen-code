import { useCallback, useEffect, useState } from "react";
import type { SettingsSectionId } from "@/lib/settingsCatalog";
import {
  buildSettingsHash,
  parseSettingsHash,
} from "@/lib/settingsCatalog";

export type AppView = "workbench" | "settings";

/** 工作台与设置页的唯一哈希路由状态。 */
export function useAppRoute() {
  const [appView, setAppView] = useState<AppView>("workbench");
  const [settingsSection, setSettingsSection] =
    useState<SettingsSectionId>("general");

  const navigateWorkbench = useCallback(() => {
    setAppView("workbench");
    if (typeof window !== "undefined" && window.location.hash) {
      window.history.replaceState(
        null,
        "",
        window.location.pathname + window.location.search,
      );
    }
  }, []);

  const navigateSettings = useCallback(
    (section: SettingsSectionId = "general") => {
      setSettingsSection(section);
      setAppView("settings");
      if (typeof window !== "undefined") {
        const hash = buildSettingsHash({ section });
        // Avoid no-op hash writes (some webviews skip hashchange; state still set above).
        if (window.location.hash !== hash) {
          window.location.hash = hash;
        }
      }
    },
    [],
  );

  // 哈希路由只接受 #/settings/{section}、#/workbench 或空路径。
  useEffect(() => {
    const syncFromHash = () => {
      const raw = (window.location.hash || "").replace(/^#\/?/, "");
      if (raw.startsWith("settings")) {
        const loc = parseSettingsHash(raw);
        if (loc) {
          setSettingsSection(loc.section);
          setAppView("settings");
        } else {
          setAppView("workbench");
          window.history.replaceState(
            null,
            "",
            window.location.pathname + window.location.search,
          );
        }
      } else if (raw === "" || raw === "workbench") {
        setAppView("workbench");
      }
    };
    syncFromHash();
    window.addEventListener("hashchange", syncFromHash);
    return () => window.removeEventListener("hashchange", syncFromHash);
  }, []);

  return { appView, settingsSection, navigateWorkbench, navigateSettings };
}

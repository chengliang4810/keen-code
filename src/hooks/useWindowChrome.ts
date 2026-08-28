import { useEffect, useMemo, useState } from "react";
import * as api from "@/lib/api";

export function useWindowChrome() {
  const platform = useMemo(() => {
    const userAgent = navigator.userAgent.toLowerCase();
    if (userAgent.includes("mac")) return "mac" as const;
    if (userAgent.includes("win")) return "win" as const;
    return "other" as const;
  }, []);
  const useCustomWindowChrome = platform !== "mac";
  const [windowMaximized, setWindowMaximized] = useState(false);
  const [windowFullscreen, setWindowFullscreen] = useState(false);

  useEffect(() => {
    document.documentElement.classList.remove("platform-mac", "platform-win", "platform-other");
    document.documentElement.classList.add(`platform-${platform}`);
  }, [platform]);

  useEffect(() => {
    if (!api.isTauri()) return;
    let unlistenResize: (() => void) | undefined;
    let cancelled = false;
    void import("@tauri-apps/api/window")
      .then(async ({ getCurrentWindow }) => {
        const window = getCurrentWindow();
        const sync = async () => {
          try {
            const [maximized, fullscreen] = await Promise.all([
              window.isMaximized(),
              window.isFullscreen(),
            ]);
            if (useCustomWindowChrome) setWindowMaximized(maximized);
            setWindowFullscreen(fullscreen);
          } catch {
            // Window state is cosmetic; retain the last known value.
          }
        };
        await sync();
        unlistenResize = await window.onResized(() => void sync());
        if (cancelled) unlistenResize();
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlistenResize?.();
    };
  }, [useCustomWindowChrome]);

  return { platform, useCustomWindowChrome, windowMaximized, windowFullscreen };
}

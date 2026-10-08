import "./styles/globals.css";

import {
  flushUiState,
  hasPendingUiState,
  hydrateUiState,
  uiState,
} from "@/lib/uiState";
import { applyLanguagePreference, t } from "@/modules/i18n/state";
import { toast } from "sonner";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import ReactDOM from "react-dom/client";
import App from "./app/App";
import { I18nBridge } from "@/modules/i18n/I18nBridge";
import { initLaunchDir } from "./lib/launchDir";
import { USE_CUSTOM_WINDOW_CONTROLS } from "./lib/platform";
import { terminalDiagnosticsEnabled } from "@/modules/terminal/lib/terminalDiagnosticsEnabled";

if (USE_CUSTOM_WINDOW_CONTROLS) {
  document.documentElement.dataset.chrome = "borderless";
}

// Render-instrumentation overlay, opt-in: `VITE_REACT_SCAN=true pnpm dev`.
// Dev-only dynamic import so it never reaches the production bundle.
if (import.meta.env.DEV && import.meta.env.VITE_REACT_SCAN === "true") {
  const { scan } = await import("react-scan");
  scan({ enabled: true });
}

try {
  await hydrateUiState();
  await applyLanguagePreference(
    (uiState.getItem("rcode-ui-language-shadow") ?? "system") as
      | "system"
      | "en-US"
      | "zh-CN",
  );
} catch (error) {
  const root = document.getElementById("root");
  if (root)
    root.textContent = t("Could not load RCode storage: {value0}", {
      value0: String(error),
    });
  await getCurrentWindow().show();
  throw error;
}

let closing = false;
await getCurrentWindow().onCloseRequested((event) => {
  if (!hasPendingUiState()) return;
  event.preventDefault();
  if (closing) return;
  closing = true;
  void flushUiState()
    .then(() => {
      closing = false;
      return getCurrentWindow().close();
    })
    .catch((error) => {
      toast.error(t("Could not save interface state"), {
        description: String(error),
      });
    })
    .finally(() => {
      closing = false;
    });
});

// Reap PTY sessions orphaned by a prior webview load before any tab spawns.
await invoke("pty_close_all").catch(() => {});

// Seed before first paint so default tab mounts at target cwd (no flicker).
await initLaunchDir();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <>
    <I18nBridge />
    <App />
  </>,
);

// Window starts hidden (per tauri.conf.json) so users never see a transparent
// shadow-only frame before React paints. Use setTimeout — rAF is throttled
// while the window is hidden and would never fire.
const showWindow = () => {
  getCurrentWindow()
    .show()
    .catch((e) => console.error("window.show failed:", e));
};
setTimeout(showWindow, 50);
// Safety net: if the first show somehow fails to take effect, force again.
setTimeout(showWindow, 500);

Object.assign(window, {
  __rcodeSetTerminalDiagnostics(enabled: boolean) {
    uiState.setItem("rcode:terminal-diagnostics", enabled ? "1" : "0");
  },
});

if (terminalDiagnosticsEnabled()) {
  void import("@/modules/terminal/lib/terminalDiagnostics").then(
    (diagnostics) => diagnostics.installTerminalDiagnostics(),
  );
}

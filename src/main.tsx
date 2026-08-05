import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles/tokens.css";
import "./styles/skins.css";
import "./styles/tailwind.css";
import "./styles/app.css";
import "./styles/setup-wizard.css";
import {
  applyNativeWindowTheme,
  applyThemeToDocument,
  getSystemTheme,
  loadThemePreference,
  resolveTheme,
} from "./lib/theme";
import {
  applySkinToDocument,
  applyWallpaperScrimToDocument,
  loadSkin,
  loadWallpaperScrim,
} from "./lib/themeSkin";
import {
  installFrontendErrorHandlers,
  reportFrontendError,
} from "./lib/frontendDiagnostics";

// React 挂载前注册，确保启动阶段与首次渲染异常也会写入统一诊断日志。
installFrontendErrorHandlers();

// Apply persisted theme preference (default: system) before first React paint.
const bootPref = loadThemePreference(localStorage);
const bootTheme = resolveTheme(bootPref, getSystemTheme());
applyThemeToDocument(bootTheme);
applySkinToDocument(loadSkin(localStorage));
applyWallpaperScrimToDocument(loadWallpaperScrim(localStorage));
// Native: null = follow OS (required for live system theme); light/dark locks chrome.
void applyNativeWindowTheme(bootPref === "system" ? null : bootTheme);

createRoot(document.getElementById("root")!, {
  /** 记录逃逸出 React 树并可能导致空白页的异常。 */
  onUncaughtError: (error, errorInfo) => {
    reportFrontendError(
      "frontend.react_uncaught",
      `${error instanceof Error ? error.stack || error.message : String(error)}\ncomponentStack=${errorInfo.componentStack ?? ""}`,
    );
  },
  /** 记录被 Error Boundary 捕获的渲染异常。 */
  onCaughtError: (error, errorInfo) => {
    reportFrontendError(
      "frontend.react_caught",
      `${error instanceof Error ? error.stack || error.message : String(error)}\ncomponentStack=${errorInfo.componentStack ?? ""}`,
    );
  },
  /** 记录 React 自动恢复但可能引起界面闪空的异常。 */
  onRecoverableError: (error, errorInfo) => {
    reportFrontendError(
      "frontend.react_recoverable",
      `${error instanceof Error ? error.stack || error.message : String(error)}\ncomponentStack=${errorInfo.componentStack ?? ""}`,
    );
  },
}).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

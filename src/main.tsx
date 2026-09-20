import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { Toaster, ToastProvider } from "@appica/ui-react/toast";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
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
import { applyUiFontSizeToDocument, loadUiFontSize } from "./lib/uiFontSize";
import { startupFrontendReady } from "./lib/api";

// React 挂载前注册，确保启动阶段与首次渲染异常也会写入统一诊断日志。
installFrontendErrorHandlers();

// Apply persisted theme preference (default: system) before first React paint.
const bootPref = loadThemePreference(localStorage);
const bootTheme = resolveTheme(bootPref, getSystemTheme());
applyThemeToDocument(bootTheme);
applySkinToDocument(loadSkin(localStorage));
applyWallpaperScrimToDocument(loadWallpaperScrim(localStorage));
// 界面字号在首次绘制前生效，避免启动时字号跳变。
applyUiFontSizeToDocument(loadUiFontSize(localStorage));
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
    <ToastProvider timeout={2000}>
      <ErrorBoundary scope="应用">
        <App />
      </ErrorBoundary>
      <Toaster position="top-center" timeout={2000} />
    </ToastProvider>
  </StrictMode>,
);

// 两帧后 DOM 已完成首次提交与一次实际绘制；失败不影响应用启动。
requestAnimationFrame(() => {
  requestAnimationFrame(() => {
    void startupFrontendReady().catch(() => {});
  });
});

import { useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  AppErrorBoundary,
  ResourceManagerApp,
  Root,
  ZCodeIntlProvider,
} from "@zcode/ui";
import "@zcode/ui/styles.css";
import { connectViaTauri } from "@zcode/client";
import {
  createTauriPlatform,
  createTauriResourceManagerBridge,
  listenResourceManagerOpen,
} from "./tauriPlatform.js";
import { installNativeWindowDrag } from "./nativeWindowDrag.js";

function DesktopResourceManagerLayer() {
  const [open, setOpen] = useState(false);
  const bridge = useMemo(() => createTauriResourceManagerBridge(), []);

  useEffect(() => listenResourceManagerOpen(() => setOpen(true)), []);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open]);

  if (!open) return null;
  return (
    <div
      className="fixed inset-0 z-[100] bg-background/95 text-foreground shadow-2xl"
      data-testid="resource-manager-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Resource manager"
    >
      <ResourceManagerApp
        getSnapshot={bridge.getSnapshot}
        storage={bridge.storage}
        onClose={() => setOpen(false)}
      />
    </div>
  );
}

async function bootstrap(): Promise<void> {
  const rootElement = document.getElementById("root");
  if (!rootElement) {
    throw new Error("缺少应用根节点");
  }

  const root = createRoot(rootElement);
  try {
    const connection = await connectViaTauri();
    const platform = createTauriPlatform();
    const disposeNativeWindowDrag = installNativeWindowDrag();
    platform.notifyRendererReady();
    root.render(
      <AppErrorBoundary>
        <ZCodeIntlProvider
          settingService={connection.services.settingService}
          broadcastService={connection.services.broadcastService}
        >
          <Root
            services={connection.services}
            platform={platform}
            isDesktop
            isWindowsDesktop={navigator.userAgent.includes("Windows")}
            restoreSession
            allowOpenWorkspace
            supportsEmbeddedBrowser
          />
          <DesktopResourceManagerLayer />
        </ZCodeIntlProvider>
      </AppErrorBoundary>,
    );
    window.addEventListener("beforeunload", () => {
      disposeNativeWindowDrag();
      void connection.protocol.close().catch((error) => {
        // 卸载阶段无法再把错误交给界面；保留关闭失败的权限/生命周期诊断，
        // 避免未处理 Promise 掩盖真实的窗口归属问题。
        console.warn("[frontend.rpc] close during unload failed", error);
      });
    }, { once: true });
  } catch (error) {
    root.render(
      <div className="flex h-dvh items-center justify-center bg-background p-6 text-foreground">
        <p className="text-ui-sm">{error instanceof Error ? error.message : String(error)}</p>
      </div>,
    );
  }
}

void bootstrap();

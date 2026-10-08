import { lazy, Suspense, useEffect, useRef } from "react";
import { Dialog as DialogPrimitive } from "radix-ui";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/localized/spinner";
import { useTranslation } from "@/modules/i18n";
import { Header } from "@/modules/header";
import { useSettingsOverlay } from "@/modules/settings/settingsOverlay";
import { useSidebarWidth } from "@/modules/sidebar/sidebarWidth";
import { loadSettingsApp } from "@/settings/loadSettingsApp";

const SettingsApp = lazy(loadSettingsApp);

export function SettingsOverlay() {
  const tr = useTranslation();
  const open = useSettingsOverlay((s) => s.open);
  const tab = useSettingsOverlay((s) => s.tab);
  const close = useSettingsOverlay((s) => s.close);
  const select = useSettingsOverlay((s) => s.select);
  const sidebarWidth = useSidebarWidth((s) => s.width);
  const returnFocus = useRef<HTMLElement | null>(null);

  useEffect(() => {
    // 首屏之后仅预热代码，不挂载设置分类或提前读取凭据。
    const preload = () => {
      void loadSettingsApp().catch((error: unknown) => {
        console.warn("Settings preload failed", error);
      });
    };
    if (typeof window.requestIdleCallback === "function") {
      const idle = window.requestIdleCallback(preload, { timeout: 1500 });
      return () => window.cancelIdleCallback(idle);
    }
    const timer = window.setTimeout(preload, 500);
    return () => window.clearTimeout(timer);
  }, []);
  return (
    <DialogPrimitive.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) close();
      }}
    >
      <DialogPrimitive.Content
        className="absolute inset-0 z-50 flex flex-col overflow-hidden bg-frame text-foreground outline-none"
        aria-describedby={undefined}
        onInteractOutside={(event) => event.preventDefault()}
        onOpenAutoFocus={() => {
          returnFocus.current =
            document.activeElement instanceof HTMLElement
              ? document.activeElement
              : null;
        }}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          if (returnFocus.current?.isConnected) returnFocus.current.focus();
        }}
      >
        <DialogPrimitive.Title className="sr-only">
          {tr("Settings")}
        </DialogPrimitive.Title>
        <Header onSettingsClick={close} settingsOpen />
        <Suspense
          fallback={
            <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-4">
              <Spinner />
              <Button variant="ghost" onClick={close}>
                {tr("Back to conversations")}
              </Button>
            </div>
          }
        >
          <SettingsApp
            active={tab}
            onTabChange={select}
            onClose={close}
            sidebarWidth={sidebarWidth}
          />
        </Suspense>
      </DialogPrimitive.Content>
    </DialogPrimitive.Root>
  );
}

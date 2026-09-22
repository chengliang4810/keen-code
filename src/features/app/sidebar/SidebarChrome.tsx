import type { KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent, RefObject } from "react";
import {
  getSidebarWidthMax,
  SIDEBAR_WIDTH_MIN,
  saveLayout,
  type LayoutPrefs,
  type SidebarResizeStart,
} from "@/lib/layout";
import type {
  SidebarSetState,
  SidebarTranslator,
} from "./types";
import { Button } from "@/components/ui/button";
import { Tip } from "@/components/ui/tooltip";
import { IconArrowLeft, IconArrowRight } from "@/components/icons";

export interface SidebarChromeProps {
  layout: LayoutPrefs;
  setLayout: SidebarSetState<LayoutPrefs>;
  setResizingSidebar: SidebarSetState<boolean>;
  sidebarRef: RefObject<HTMLElement | null>;
  sidebarResizeStartRef: RefObject<SidebarResizeStart | null>;
  canGoBack: boolean;
  canGoForward: boolean;
  goBack: () => Promise<void>;
  goForward: () => Promise<void>;
  useCustomWindowChrome: boolean;
  toggleMaximizeFromTitlebar: () => Promise<void>;
  tr: SidebarTranslator;
}

export function SidebarChrome({
  layout,
  setLayout,
  setResizingSidebar,
  sidebarRef,
  sidebarResizeStartRef,
  canGoBack,
  canGoForward,
  goBack,
  goForward,
  useCustomWindowChrome,
  toggleMaximizeFromTitlebar,
  tr,
}: SidebarChromeProps) {
  const getAvailableWidth = () => {
    const measured = sidebarRef.current?.parentElement?.getBoundingClientRect().width;
    if (measured && measured > 0) return measured;
    return typeof window === "undefined"
      ? Number.POSITIVE_INFINITY
      : window.innerWidth;
  };

  return (
    <>
      {!layout.sidebarCollapsed && (
        <div
          className="sidebar-resizer"
          role="separator"
          aria-orientation="vertical"
          aria-label={tr("main.resizeLeftPane")}
          tabIndex={0}
          aria-valuemin={SIDEBAR_WIDTH_MIN}
          aria-valuemax={
            getSidebarWidthMax(getAvailableWidth())
          }
          aria-valuenow={layout.sidebarWidth}
          onKeyDown={(event: ReactKeyboardEvent<HTMLDivElement>) => {
            const max = getSidebarWidthMax(getAvailableWidth());
            const nextWidth =
              event.key === "Home"
                ? SIDEBAR_WIDTH_MIN
                : event.key === "End"
                  ? max
                  : event.key === "ArrowLeft"
                    ? Math.max(SIDEBAR_WIDTH_MIN, layout.sidebarWidth - 16)
                    : event.key === "ArrowRight"
                      ? Math.min(max, layout.sidebarWidth + 16)
                      : null;
            if (nextWidth == null) return;
            event.preventDefault();
            setLayout((current) => {
              const next = { ...current, sidebarWidth: nextWidth };
              saveLayout(localStorage, next);
              return next;
            });
          }}
          onPointerDown={(event: ReactPointerEvent<HTMLDivElement>) => {
            event.preventDefault();
            sidebarResizeStartRef.current = {
              clientX: event.clientX,
              width: layout.sidebarWidth,
            };
            event.currentTarget.setPointerCapture?.(event.pointerId);
            setResizingSidebar(true);
          }}
        />
      )}
      <div
        className="sidebar-chrome"
        data-tauri-drag-region
        onDoubleClick={() => {
          if (useCustomWindowChrome) void toggleMaximizeFromTitlebar();
        }}
      >
        <Tip label={tr("main.leftPaneHide")}>
          <Button
            type="button"
            variant="ghost"
            size="md"
            className="sidebar-brand"
            aria-label={tr("main.leftPaneHide")}
            onClick={() =>
              setLayout((current) => {
                const next = { ...current, sidebarCollapsed: true };
                saveLayout(localStorage, next);
                return next;
              })
            }
          >
            <img
              src="/logo.png"
              alt=""
              className="sidebar-brand__logo"
              draggable={false}
            />
          </Button>
        </Tip>
        <div
          className="sidebar-chrome__task-nav"
          data-testid="sidebar-task-navigation"
        >
          <Tip label={tr("resources.browserBack")}>
            <Button
              type="button"
              variant="ghost"
              size="md"
              className="sidebar-chrome__icon-button"
              aria-label={tr("resources.browserBack")}
              disabled={!canGoBack}
              onClick={() => void goBack()}
            >
              <IconArrowLeft size={16} />
            </Button>
          </Tip>
          <Tip label={tr("resources.browserForward")}>
            <Button
              type="button"
              variant="ghost"
              size="md"
              className="sidebar-chrome__icon-button"
              aria-label={tr("resources.browserForward")}
              disabled={!canGoForward}
              onClick={() => void goForward()}
            >
              <IconArrowRight size={16} />
            </Button>
          </Tip>
        </div>
        <div className="sidebar-chrome__drag" data-tauri-drag-region />
      </div>
    </>
  );
}

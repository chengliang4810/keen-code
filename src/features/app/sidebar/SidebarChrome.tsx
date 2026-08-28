import type { LayoutPrefs } from "@/lib/layout";
import type {
  SidebarSetState,
  SidebarTranslator,
} from "./types";
import { saveLayout } from "@/lib/layout";
import { Button } from "@/components/ui/button";
import { Tip } from "@/components/ui/tooltip";
import { IconPanel } from "@/components/icons";

export interface SidebarChromeProps {
  layout: LayoutPrefs;
  setLayout: SidebarSetState<LayoutPrefs>;
  setResizingSidebar: SidebarSetState<boolean>;
  useCustomWindowChrome: boolean;
  toggleMaximizeFromTitlebar: () => Promise<void>;
  tr: SidebarTranslator;
}

export function SidebarChrome({
  layout,
  setLayout,
  setResizingSidebar,
  useCustomWindowChrome,
  toggleMaximizeFromTitlebar,
  tr,
}: SidebarChromeProps) {
  return (
    <>
      {!layout.sidebarCollapsed && (
        <div
          className="sidebar-resizer"
          role="separator"
          aria-orientation="vertical"
          aria-label={tr("main.resizeLeftPane")}
          onPointerDown={(event) => {
            event.preventDefault();
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
            className="chrome-btn chrome-btn--traffic main__pane-toggle is-on"
            onClick={() =>
              setLayout((current) => {
                const next = { ...current, sidebarCollapsed: true };
                saveLayout(localStorage, next);
                return next;
              })
            }
          >
            <IconPanel size={16} />
          </Button>
        </Tip>
        <div className="sidebar-chrome__drag" data-tauri-drag-region />
      </div>
    </>
  );
}

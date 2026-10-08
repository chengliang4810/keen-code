import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import { WindowControls } from "@/components/WindowControls";
import { IS_MAC, USE_CUSTOM_WINDOW_CONTROLS } from "@/lib/platform";
import {
  Settings01Icon,
  SidebarLeftIcon,
  SidebarRightIcon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";

type Props = {
  onToggleSidebar?: () => void;
  onSettingsClick: () => void;
  settingsOpen?: boolean;
  toolsOpen?: boolean;
  onToggleTools?: () => void;
};

export function Header({
  onToggleSidebar,
  onSettingsClick,
  settingsOpen = false,
  toolsOpen = true,
  onToggleTools,
}: Props) {
  const tr = useTranslation();

  return (
    <div
      data-tauri-drag-region
      className={`rcode-header flex shrink-0 items-center gap-2 select-none ${
        IS_MAC ? "pr-2 pl-20" : "pr-0 pl-2"
      }`}
    >
      {onToggleSidebar && (
        <div className="flex shrink-0 items-center gap-0.5">
          <Button
            onClick={onToggleSidebar}
            aria-label={tr("Toggle sidebar")}
            title={tr("Toggle sidebar")}
            variant="ghost"
            size="icon-sm"
            className="shrink-0 rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-inset active:not-aria-[haspopup]:translate-y-0"
          >
            <HugeiconsIcon
              icon={SidebarLeftIcon}
              size={18}
              className="size-[18px]"
              strokeWidth={1.75}
            />
          </Button>
        </div>
      )}

      <div
        className="flex min-w-0 flex-1 items-center gap-2"
        data-tauri-drag-region
        onContextMenu={(e) => {
          // Empty chrome falls through to WebKit Reload, which reloads the
          // webview and pty_close_all's every shell (#1242). Per-tab menus
          // still open: Radix handles the trigger before this bubbles.
          e.preventDefault();
        }}
      >
        <div data-tauri-drag-region className="h-full min-w-2 flex-1" />
      </div>

      <Button
        variant="ghost"
        size="icon-sm"
        onClick={onSettingsClick}
        aria-label={tr("Settings")}
        title={tr("Settings")}
        aria-haspopup="dialog"
        aria-expanded={settingsOpen}
        className="shrink-0 rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-inset active:not-aria-[haspopup]:translate-y-0"
      >
        <HugeiconsIcon
          icon={Settings01Icon}
          size={18}
          className="size-[18px]"
          strokeWidth={1.75}
        />
      </Button>
      {onToggleTools ? (
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={onToggleTools}
          aria-label={tr(
            toolsOpen ? "Hide right sidebar" : "Show right sidebar",
          )}
          title={tr(toolsOpen ? "Hide right sidebar" : "Show right sidebar")}
          aria-pressed={toolsOpen}
          className="shrink-0 rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-inset active:not-aria-[haspopup]:translate-y-0"
        >
          <HugeiconsIcon
            icon={SidebarRightIcon}
            size={18}
            className="size-[18px]"
            strokeWidth={1.75}
          />
        </Button>
      ) : (
        <div aria-hidden="true" className="size-8 shrink-0" />
      )}

      {USE_CUSTOM_WINDOW_CONTROLS && (
        <>
          <span className="ml-1 h-5 w-px shrink-0 bg-border/60" />
          <WindowControls />
        </>
      )}
    </div>
  );
}

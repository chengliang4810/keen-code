import { UnifiedBrowserView } from "@/browser-use/UnifiedBrowserView.js";
import { cn } from "@/components/lib/utils.js";
import { TabsContent } from "@/components/ui/tabs.js";
import type { BrowserSidePaneMetadata, BrowserUseSidePaneTab } from "@/lib/workspaceSidePane.js";

interface BrowserUseSidePaneContentProps {
  tab: BrowserUseSidePaneTab;
  isPanelVisible: boolean;
  isSelected: boolean;
  isCurrentTask: boolean;
  initialUrl?: string;
  workspacePath: string;
  workspaceIdentity?: string;
  residencyGeneration?: number;
  onUrlChange(url: string): void;
  onOpenBrowserUrl?(url: string): void;
  onPageMetadataChange(metadata: BrowserSidePaneMetadata): void;
}

/** browser-use 专用 TabsContent：非活动 tab 仅在截图准备期间保留真实合成布局。 */
export function BrowserUseSidePaneContent({
  tab,
  isPanelVisible,
  isSelected,
  isCurrentTask,
  initialUrl,
  workspacePath,
  workspaceIdentity,
  residencyGeneration,
  onUrlChange,
  onOpenBrowserUrl,
  onPageMetadataChange,
}: BrowserUseSidePaneContentProps): React.JSX.Element {
  return (
    <TabsContent
      value={tab.id}
      forceMount
      aria-hidden={!isSelected}
      inert={!isSelected ? true : undefined}
      data-browser-use-tab-id={tab.tabId}
      className={cn(
        "h-full min-h-0 bg-background",
        isSelected ? "relative z-10 flex" : "hidden",
      )}
    >
      <UnifiedBrowserView
        browserKey={tab.tabId}
        isResidencyRestore={tab.residency === "restoring"}
        isVisible={isPanelVisible && isSelected}
        isSelected={isSelected}
        isCurrentTask={isCurrentTask}
        initialUrl={initialUrl}
        faviconUrl={tab.faviconUrl}
        workspacePath={workspacePath}
        workspaceIdentity={workspaceIdentity}
        workspaceKey={tab.workspaceKey ?? (workspaceIdentity?.trim() || workspacePath)}
        remoteSessionId={tab.remoteSessionId ?? undefined}
        sessionId={tab.sessionId}
        browserGeneration={tab.browserGeneration}
        residencyGeneration={tab.residencyGeneration ?? residencyGeneration}
        browserUseOperationUntil={tab.browserUseOperationUntil}
        browserResizeBaselineVersion={tab.browserUseResizeBaselineVersion}
        onUrlChange={onUrlChange}
        onOpenBrowserUrl={onOpenBrowserUrl}
        onPageMetadataChange={onPageMetadataChange}
      />
    </TabsContent>
  );
}

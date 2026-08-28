import type { Dispatch, RefObject, SetStateAction } from "react";
import type { SessionSnapshot, ChatMessage } from "@/lib/session";
import type { AcpSessionView, AcpSubagentInfo } from "@/lib/acp/store";
import type { Locale } from "@/i18n";
import {
  ResourceViewer,
  type ResourceOpenTarget,
  type ResourceViewerProps,
} from "@/components/ResourceViewer";
import type { Project } from "@/features/app/models";
import { saveLayout, type LayoutPrefs } from "@/lib/layout";

export interface ResourceAsideProps {
  asideRef: RefObject<HTMLElement | null>;
  layout: LayoutPrefs;
  setLayout: Dispatch<SetStateAction<LayoutPrefs>>;
  resizingAside: boolean;
  setResizingAside: Dispatch<SetStateAction<boolean>>;
  resourceOpenTarget: ResourceOpenTarget | null;
  setResourceOpenTarget: Dispatch<SetStateAction<ResourceOpenTarget | null>>;
  activeProject: Project | null;
  session: SessionSnapshot;
  messages: ChatMessage[];
  locale: Locale;
  resourceSyncRevision: number;
  acpSessionView: AcpSessionView | null;
  displayedSubagents: AcpSubagentInfo[];
  subagentModelLabels: Record<string, string>;
  terminalFontFamily: string;
  modelLabel: string;
  loadTrajectoryMessages: NonNullable<
    ResourceViewerProps["onLoadTrajectoryMessages"]
  >;
}

export function ResourceAside({
  asideRef,
  layout,
  setLayout,
  resizingAside,
  setResizingAside,
  resourceOpenTarget,
  setResourceOpenTarget,
  activeProject,
  session,
  messages,
  locale,
  resourceSyncRevision,
  acpSessionView,
  displayedSubagents,
  subagentModelLabels,
  terminalFontFamily,
  modelLabel,
  loadTrajectoryMessages,
}: ResourceAsideProps) {
  return (
        <aside
          ref={asideRef}
          className={
            (layout.asideCollapsed ? "aside aside--hidden" : "aside") +
            (resizingAside ? " is-resizing" : "")
          }
          aria-hidden={layout.asideCollapsed}
          style={
            !layout.asideCollapsed
              ? {
                  width: layout.asideWidth,
                  minWidth: layout.asideWidth,
                  maxWidth: layout.asideWidth,
                }
              : undefined
          }
        >
          {!layout.asideCollapsed && (
            <div
              className="aside-resizer"
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize files pane"
              onPointerDown={(e) => {
                e.preventDefault();
                setResizingAside(true);
              }}
            />
          )}
          <div className="aside__inner">
            <ResourceViewer
              sessionKey={session.sessionId ?? "__draft__"}
              projectPath={activeProject?.path ?? null}
              projectName={activeProject?.name ?? null}
              locale={locale}
              terminalFontFamily={terminalFontFamily}
              paneActive={!layout.asideCollapsed}
              onTabsEmpty={() =>
                setLayout((current) => {
                  if (current.asideCollapsed) return current;
                  const next = { ...current, asideCollapsed: true };
                  saveLayout(localStorage, next);
                  return next;
                })
              }
              syncRevision={resourceSyncRevision}
              openRequest={resourceOpenTarget}
              onOpenRequestConsumed={() => setResourceOpenTarget(null)}
              trajectoryLive={{
                sessionId: session.sessionId ?? null,
                title: acpSessionView?.title ?? null,
                messages,
                subagents: displayedSubagents,
              }}
              subagents={displayedSubagents}
              modelLabel={modelLabel}
              subagentModelLabels={subagentModelLabels}
              onLoadTrajectoryMessages={loadTrajectoryMessages}
            />
          </div>
        </aside>
  );
}

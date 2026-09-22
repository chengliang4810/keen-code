import type { Project, SessionRow } from "@/features/app/models";
import { VirtualList } from "@/components/VirtualList";
import {
  SIDEBAR_SESSION_ROW_GAP,
  SIDEBAR_SESSION_ROW_HEIGHT,
  SIDEBAR_TOUCH_SESSION_ROW_HEIGHT,
} from "@/lib/virtualList";
import { SidebarSessionRow } from "./SidebarSessionRow";
import type {
  SidebarSetState,
  SidebarSessionActions,
  SidebarSessionStatus,
  SidebarTranslator,
} from "./types";

export interface PinnedSessionListProps
  extends SidebarSessionActions,
    SidebarSessionStatus {
  tr: SidebarTranslator;
  pinnedSessions: SessionRow[];
  pinnedOpen: boolean;
  setPinnedOpen: SidebarSetState<boolean>;
  projects: Project[];
}

export function PinnedSessionList({
  tr,
  pinnedSessions,
  session,
  busyIds,
  unreadTerminalResults,
  projects,
  pendingAskUserSessionIds,
  startSidebarDrag,
  endSidebarDrag,
  dropSession,
  openSession,
  openSessionMenu,
  archiveSession,
  pinSession,
}: PinnedSessionListProps) {
  if (pinnedSessions.length === 0) return null;

  return (
    <>
      <div className="tree-l1">
        <div className="tree-l1__head" role="heading" aria-level={2}>
          <span className="tree-l1__label">{tr("sidebar.pinned")}</span>
        </div>
      </div>
      <VirtualList
        className="tree-orphan-list"
        items={pinnedSessions}
        getKey={(item) => item.id}
        rowHeight={SIDEBAR_SESSION_ROW_HEIGHT}
        touchRowHeight={SIDEBAR_TOUCH_SESSION_ROW_HEIGHT}
        gap={SIDEBAR_SESSION_ROW_GAP}
        scrollToKey={
          session.sessionId &&
          pinnedSessions.some((item) => item.id === session.sessionId)
            ? session.sessionId
            : null
        }
        renderItem={(item) => {
          const project = item.projectId
            ? projects.find((candidate) => candidate.id === item.projectId) ??
              null
            : null;
          return (
            <SidebarSessionRow
              tr={tr}
              startSidebarDrag={startSidebarDrag}
              endSidebarDrag={endSidebarDrag}
              dropSession={dropSession}
              openSession={openSession}
              openSessionMenu={openSessionMenu}
              archiveSession={archiveSession}
              pinSession={pinSession}
              session={item}
              project={project}
              activeSessionId={session.sessionId}
              working={busyIds.has(item.id)}
              loading={
                session.sessionId === item.id && session.state === "connecting"
              }
              unreadResult={unreadTerminalResults.get(item.id) ?? null}
              needsInput={pendingAskUserSessionIds.has(item.id)}
              variant="pinned"
            />
          );
        }}
      />
    </>
  );
}

import { useMemo } from "react";
import type { Project, SessionRow } from "@/features/app/models";
import { VirtualList } from "@/components/VirtualList";
import { IconArchive } from "@/components/icons";
import {
  SIDEBAR_ARCHIVED_SESSION_ROW_HEIGHT,
  SIDEBAR_SESSION_ROW_GAP,
  SIDEBAR_TOUCH_ARCHIVED_SESSION_ROW_HEIGHT,
} from "@/lib/virtualList";
import { sortSessionRows, type SidebarSortMode } from "@/lib/sidebarOrder";
import { SidebarSessionRow } from "./SidebarSessionRow";
import type {
  SidebarSessionActions,
  SidebarSessionStatus,
  SidebarTranslator,
} from "./types";

export interface ArchivedSessionListProps
  extends SidebarSessionActions,
    SidebarSessionStatus {
  tr: SidebarTranslator;
  /** Archive mode receives the complete loaded list, including project sessions. */
  sessions: SessionRow[];
  projects: Project[];
  sessionOrder: string[];
  sessionSortMode: SidebarSortMode;
  deleteArchivedSession: (sessionId: string) => void;
}

/** ZCode-style flat archive view; restoring delegates to the existing archive action. */
export function ArchivedSessionList({
  tr,
  sessions,
  projects,
  sessionOrder,
  sessionSortMode,
  deleteArchivedSession,
  session,
  busyIds,
  unreadTerminalResults,
  pendingAskUserSessionIds,
  startSidebarDrag,
  endSidebarDrag,
  dropSession,
  openSession,
  openSessionMenu,
  archiveSession,
  pinSession,
}: ArchivedSessionListProps) {
  const archivedSessions = useMemo(
    () =>
      sortSessionRows(
        sessions.filter((item) => item.archived),
        sessionOrder,
        sessionSortMode,
      ),
    [sessionOrder, sessionSortMode, sessions],
  );

  return (
    <>
      <div className="tree-l1 tree-l1--top-spaced">
        <div className="tree-l1__head" role="heading" aria-level={2}>
          <span className="tree-l1__label">
            <IconArchive size={14} />
            {tr("sidebar.archived")}
          </span>
        </div>
      </div>
      {archivedSessions.length === 0 ? (
        <div className="sidebar-empty">{tr("sidebar.noArchivedSessions")}</div>
      ) : (
        <VirtualList
          className="tree-orphan-list"
          items={archivedSessions}
          getKey={(item) => item.id}
          rowHeight={SIDEBAR_ARCHIVED_SESSION_ROW_HEIGHT}
          touchRowHeight={SIDEBAR_TOUCH_ARCHIVED_SESSION_ROW_HEIGHT}
          gap={SIDEBAR_SESSION_ROW_GAP}
          scrollToKey={
            session.sessionId &&
            archivedSessions.some((item) => item.id === session.sessionId)
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
                deleteSession={() => deleteArchivedSession(item.id)}
                session={item}
                project={project}
                activeSessionId={session.sessionId}
                working={busyIds.has(item.id)}
                loading={session.sessionId === item.id && session.state === "connecting"}
                unreadResult={unreadTerminalResults.get(item.id) ?? null}
                needsInput={pendingAskUserSessionIds.has(item.id)}
                variant="archived"
              />
            );
          }}
        />
      )}
    </>
  );
}

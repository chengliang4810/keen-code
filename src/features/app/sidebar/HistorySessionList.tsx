import type { SessionRow } from "@/features/app/models";
import { Button } from "@/components/ui/button";
import { VirtualList } from "@/components/VirtualList";
import { IconChevronDown } from "@/components/icons";
import {
  SIDEBAR_SESSION_ROW_GAP,
  SIDEBAR_SESSION_ROW_HEIGHT,
} from "@/lib/virtualList";
import { SidebarSessionRow } from "./SidebarSessionRow";
import type {
  SidebarSetState,
  SidebarSessionActions,
  SidebarSessionStatus,
  SidebarTranslator,
} from "./types";

export interface HistorySessionListProps
  extends SidebarSessionActions,
    SidebarSessionStatus {
  tr: SidebarTranslator;
  orphanSessions: SessionRow[];
  historyOpen: boolean;
  setHistoryOpen: SidebarSetState<boolean>;
}

export function HistorySessionList({
  tr,
  orphanSessions,
  historyOpen,
  setHistoryOpen,
  session,
  busyIds,
  completedUnreadIds,
  pendingAskUserSessionIds,
  startSidebarDrag,
  endSidebarDrag,
  dropSession,
  openSession,
  openSessionMenu,
  archiveSession,
  pinSession,
}: HistorySessionListProps) {
  if (orphanSessions.length === 0) return null;

  return (
    <>
      <div className="tree-l1" style={{ marginTop: 8 }}>
        <Button
          type="button"
          className="tree-l1__head"
          onClick={() => setHistoryOpen((value) => !value)}
          aria-expanded={historyOpen}
        >
          <span className="tree-l1__label">
            {tr("sidebar.otherSessions")}
          </span>
          <IconChevronDown size={14} className="chevron--disclose" />
        </Button>
      </div>
      {historyOpen ? (
        <VirtualList
          className="tree-orphan-list"
          items={orphanSessions}
          getKey={(item) => item.id}
          rowHeight={SIDEBAR_SESSION_ROW_HEIGHT}
          gap={SIDEBAR_SESSION_ROW_GAP}
          scrollToKey={
            session.sessionId &&
            orphanSessions.some((item) => item.id === session.sessionId)
              ? session.sessionId
              : null
          }
          renderItem={(item) => (
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
              project={null}
              activeSessionId={session.sessionId}
              working={busyIds.has(item.id)}
              completedUnread={completedUnreadIds.has(item.id)}
              needsInput={pendingAskUserSessionIds.has(item.id)}
              variant="history"
            />
          )}
        />
      ) : null}
    </>
  );
}

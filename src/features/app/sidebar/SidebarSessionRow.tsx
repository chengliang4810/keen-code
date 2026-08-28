import type { MouseEvent as ReactMouseEvent } from "react";
import type { Project, SessionRow } from "@/features/app/models";
import { Button } from "@/components/ui/button";
import { Tip } from "@/components/ui/tooltip";
import { Spinner } from "@/components/ui/spinner";
import {
  IconArchive,
  IconMore,
  IconPin,
  IconPinOff,
} from "@/components/icons";
import type { SidebarSessionActions, SidebarTranslator } from "./types";

export type SidebarSessionRowVariant = "pinned" | "project" | "history";

export interface SidebarSessionRowProps extends SidebarSessionActions {
  tr: SidebarTranslator;
  session: SessionRow;
  project: Project | null;
  activeSessionId: string | null;
  working: boolean;
  completedUnread: boolean;
  needsInput: boolean;
  variant: SidebarSessionRowVariant;
}

function stopPropagation(event: ReactMouseEvent) {
  event.stopPropagation();
}

export function SidebarSessionRow({
  tr,
  startSidebarDrag,
  endSidebarDrag,
  dropSession,
  openSession,
  openSessionMenu,
  archiveSession,
  pinSession,
  session,
  project,
  activeSessionId,
  working,
  completedUnread,
  needsInput,
  variant,
}: SidebarSessionRowProps) {
  const isPinnedSection = variant === "pinned";
  const isOrphan = variant !== "project";
  const archiveLabel =
    isPinnedSection || variant === "history"
      ? tr("sidebar.archive")
      : session.archived
        ? tr("sidebar.unarchive")
        : tr("sidebar.archive");
  const pinLabel = session.pinned
    ? tr("session.unpin")
    : tr("session.pin");

  return (
    <div
      draggable
      onDragStart={(event) => startSidebarDrag(event, "session", session.id)}
      onDragEnd={endSidebarDrag}
      onDragOver={(event) => event.preventDefault()}
      onDrop={(event) => dropSession(event, session.id)}
      className={
        "tree-l3" +
        (isOrphan ? " tree-l3--orphan" : "") +
        (activeSessionId === session.id ? " tree-l3--active" : "") +
        (session.archived ? " tree-l3--archived" : "") +
        (working ? " tree-l3--working" : "") +
        (needsInput ? " tree-l3--needs-input" : "") +
        (completedUnread ? " tree-l3--completed-unread" : "")
      }
      role="button"
      tabIndex={0}
      onClick={() => void openSession(session, project)}
      onContextMenu={(event) => openSessionMenu(event, session)}
      onKeyDown={(event) => {
        if (event.key === "Enter") void openSession(session, project);
      }}
    >
      <span className="tree-l3__title">
        {isPinnedSection || session.pinned ? (
          <span
            className="tree-l3__kind"
            title={tr("session.pinned")}
            aria-label={tr("session.pinned")}
          >
            <IconPin size={12} className="tree-l3__pin" />
          </span>
        ) : null}
        <span className="tree-l3__name">{session.title || "Untitled"}</span>
        {needsInput ? (
          <span className="tree-l3__input-badge">
            {tr("sidebar.needsUserInput")}
          </span>
        ) : null}
      </span>
      {working ? (
        isPinnedSection ? (
          <Spinner size={14} className="tree-l3__spinner" />
        ) : (
          <Tip label={tr("sidebar.sessionWorking")}>
            <span
              className="tree-l3__status"
              aria-label={tr("sidebar.sessionWorking")}
            >
              <Spinner size={14} className="tree-l3__spinner" />
            </span>
          </Tip>
        )
      ) : (
        <>
          {completedUnread ? (
            <Tip label={tr("sidebar.sessionCompletedUnread")}>
              <span
                className="tree-l3__status tree-l3__status--completed"
                aria-label={tr("sidebar.sessionCompletedUnread")}
              >
                <span className="tree-l3__completion-dot" />
              </span>
            </Tip>
          ) : null}
          <span className="tree-l3__actions tree-l3__actions--triple">
            <Tip label={pinLabel}>
              <Button
                type="button"
                className="tree-icon-btn"
                onClick={(event) => {
                  stopPropagation(event);
                  void pinSession(session, !session.pinned);
                }}
              >
                {session.pinned ? (
                  <IconPinOff size={13} />
                ) : (
                  <IconPin size={13} />
                )}
              </Button>
            </Tip>
            <Tip label={archiveLabel}>
              <Button
                type="button"
                className="tree-icon-btn"
                onClick={(event) => {
                  stopPropagation(event);
                  void archiveSession(session, !session.archived);
                }}
              >
                <IconArchive size={13} />
              </Button>
            </Tip>
            {variant === "history" ? (
              <Button
                type="button"
                className="tree-icon-btn"
                onClick={(event) => openSessionMenu(event, session)}
              >
                <IconMore size={13} />
              </Button>
            ) : (
              <Tip label={tr("sidebar.menu")}>
                <Button
                  type="button"
                  className="tree-icon-btn"
                  onClick={(event) => openSessionMenu(event, session)}
                >
                  <IconMore size={13} />
                </Button>
              </Tip>
            )}
          </span>
        </>
      )}
    </div>
  );
}

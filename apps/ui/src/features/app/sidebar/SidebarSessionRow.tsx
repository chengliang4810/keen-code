import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import type { Project, SessionRow } from "@/features/app/models";
import { Button } from "@appica/ui-react/button";
import { Tip } from "@/components/ui/tooltip";
import { Spinner } from "@appica/ui-react/spinner";
import { Badge } from "@appica/ui-react/badge";
import { TaskTitleOverflowText } from "@/components/TaskTitleOverflowText";
import type { UnreadTerminalResult } from "@/lib/sessionCompletion";
import {
  IconArchive,
  IconMore,
  IconPin,
  IconPinOff,
  IconTrash,
} from "@/components/icons";
import { formatSessionRelativeTime } from "@/lib/sessionRelativeTime";
import type { SidebarSessionActions, SidebarTranslator } from "./types";

export type SidebarSessionRowVariant =
  | "pinned"
  | "project"
  | "history"
  | "archived";

interface SidebarSessionRowProps extends SidebarSessionActions {
  tr: SidebarTranslator;
  session: SessionRow;
  project: Project | null;
  activeSessionId: string | null;
  working: boolean;
  loading?: boolean;
  unreadResult: UnreadTerminalResult | null;
  needsInput: boolean;
  variant: SidebarSessionRowVariant;
  /** 归档行的永久删除入口；确认和 ACP 调用由生命周期 hook 负责。 */
  deleteSession?: (session: SessionRow) => void;
}

function stopPropagation(event: ReactMouseEvent) {
  event.stopPropagation();
}

function isNoHoverDevice() {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(hover: none), (pointer: coarse)").matches
  );
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
  loading = false,
  unreadResult,
  needsInput,
  variant,
  deleteSession,
}: SidebarSessionRowProps) {
  const isOrphan =
    variant === "history" || (variant === "archived" && project === null);
  const archiveLabel =
    variant === "archived" || session.archived
      ? tr("sidebar.unarchive")
      : tr("sidebar.archive");
  const pinLabel = session.pinned
    ? tr("session.unpin")
    : tr("session.pin");
  const relativeTime = formatSessionRelativeTime(session.updatedAt, tr);
  const rowRef = useRef<HTMLDivElement | null>(null);
  const [hovered, setHovered] = useState(false);
  const [focusWithin, setFocusWithin] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  // 触屏没有 hover，动作簇必须直接挂载；桌面端则只在交互时挂载，避免隐藏按钮进入 Tab 顺序。
  const [noHoverDevice] = useState(isNoHoverDevice);
  const actionsVisible = noHoverDevice || hovered || focusWithin || menuOpen;
  const metadataSuppressed = hovered || focusWithin || menuOpen;
  const canDelete = variant === "archived" && deleteSession !== undefined;

  useEffect(() => {
    if (!menuOpen) return;

    const closeOnPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && rowRef.current?.contains(target)) return;
      setMenuOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    const closeOnWindowBlur = () => setMenuOpen(false);

    // ContextMenu 在 portal 中打开，行本身收不到菜单关闭回调；监听外部指针、Esc 和窗口失焦
    // 只维持本行的视觉状态，不参与菜单自身的关闭逻辑。
    window.addEventListener("pointerdown", closeOnPointerDown, true);
    window.addEventListener("keydown", closeOnEscape, true);
    window.addEventListener("blur", closeOnWindowBlur);
    return () => {
      window.removeEventListener("pointerdown", closeOnPointerDown, true);
      window.removeEventListener("keydown", closeOnEscape, true);
      window.removeEventListener("blur", closeOnWindowBlur);
    };
  }, [menuOpen]);

  const openMenu = (event: ReactMouseEvent) => {
    stopPropagation(event);
    setMenuOpen(true);
    openSessionMenu(event, session);
  };

  return (
    // The row contains nested action buttons, so it cannot use Button without invalid nested controls.
    <div
      ref={rowRef}
      draggable
      data-task-item-key={session.id}
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
        (unreadResult ? " tree-l3--unread-terminal" : "") +
        (actionsVisible ? " tree-l3--actions-visible" : "") +
        (metadataSuppressed ? " tree-l3--metadata-suppressed" : "") +
        (menuOpen ? " tree-l3--menu-open" : "")
      }
      role="button"
      tabIndex={0}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      onFocusCapture={() => setFocusWithin(true)}
      onBlurCapture={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
          setFocusWithin(false);
        }
      }}
      onClick={() => {
        setMenuOpen(false);
        void openSession(session, project);
      }}
      onContextMenu={(event) => {
        setMenuOpen(true);
        openSessionMenu(event, session);
      }}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          setMenuOpen(false);
          void openSession(session, project);
        }
      }}
    >
      {/* 状态槽固定在左侧；动作显隐和 metadata 隐藏不能让状态消失。 */}
      <span className="tree-l3__leading tree-l3__kind">
        {working || loading ? (
          <Tip label={tr("sidebar.sessionWorking")}>
            <span
              className="tree-l3__status tree-l3__status--loading"
              aria-label={tr("sidebar.sessionWorking")}
            >
              <Spinner currentColor className="tree-l3__spinner text-sm" />
            </span>
          </Tip>
        ) : unreadResult ? (
          <Tip
            label={
              unreadResult === "failed"
                ? tr("sidebar.sessionFailedUnread")
                : tr("sidebar.sessionCompletedUnread")
            }
          >
            <span
              className={
                "tree-l3__status tree-l3__status--" +
                (unreadResult === "failed" ? "failed" : "completed")
              }
              aria-label={
                unreadResult === "failed"
                  ? tr("sidebar.sessionFailedUnread")
                  : tr("sidebar.sessionCompletedUnread")
              }
            >
              <span className="tree-l3__completion-dot" />
            </span>
          </Tip>
        ) : session.pinned ? (
          <span
            className="tree-l3__pinned-state"
            title={tr("session.pinned")}
            aria-label={tr("session.pinned")}
          >
            <IconPin size={13} />
          </span>
        ) : null}
      </span>
      <span className="tree-l3__title">
        <span className="tree-l3__title-line">
          <TaskTitleOverflowText className="tree-l3__name">
            {session.title || "Untitled"}
          </TaskTitleOverflowText>
          {needsInput ? (
            <Badge size="md" variant="warning">
              {tr("sidebar.needsUserInput")}
            </Badge>
          ) : null}
        </span>
        {variant === "archived" ? (
          <span className="tree-l3__archive-meta">
            <span
              className="tree-l3__archive-workspace"
              title={project?.path ?? undefined}
            >
              {project?.name ?? tr("settings.archived.noProject")}
            </span>
            {relativeTime ? <span aria-hidden="true">·</span> : null}
            {relativeTime ? (
              <time
                className="tree-l3__archive-time"
                dateTime={session.updatedAt}
                aria-label={relativeTime}
              >
                {relativeTime}
              </time>
            ) : null}
          </span>
        ) : null}
      </span>
      <span className="tree-l3__trailing">
        <span className="tree-l3__meta">
          {variant !== "archived" && relativeTime ? (
            <time
              className="tree-l3__time"
              dateTime={session.updatedAt}
              aria-label={relativeTime}
            >
              {relativeTime}
            </time>
          ) : null}
        </span>
        {actionsVisible ? (
          <span
            className={
              "tree-l3__actions" +
              (!noHoverDevice ? " tree-l3__actions--with-pin" : "") +
              (canDelete ? " tree-l3__actions--with-delete" : "")
            }
          >
            {!noHoverDevice ? (
              <>
                <Tip label={pinLabel}>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    className="tree-icon-btn tree-l3__pin-action"
                    tabIndex={actionsVisible ? 0 : -1}
                    onClick={(event) => {
                      stopPropagation(event);
                      void pinSession(session, !session.pinned);
                    }}
                  >
                    {session.pinned ? <IconPinOff size={13} /> : <IconPin size={13} />}
                  </Button>
                </Tip>
                <Tip label={archiveLabel}>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    className="tree-icon-btn"
                    tabIndex={actionsVisible ? 0 : -1}
                    onClick={(event) => {
                      stopPropagation(event);
                      void archiveSession(session, !session.archived);
                    }}
                  >
                    <IconArchive size={13} />
                  </Button>
                </Tip>
              </>
            ) : null}
            {canDelete ? (
              <Tip label={tr("settings.archived.delete")}>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  className="tree-icon-btn tree-l3__delete-action"
                  tabIndex={actionsVisible ? 0 : -1}
                  onClick={(event) => {
                    stopPropagation(event);
                    deleteSession(session);
                  }}
                >
                  <IconTrash size={13} />
                </Button>
              </Tip>
            ) : null}
            {variant === "history" ? (
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                className="tree-icon-btn"
                tabIndex={actionsVisible ? 0 : -1}
                onClick={openMenu}
              >
                <IconMore size={13} />
              </Button>
            ) : (
              <Tip label={tr("sidebar.menu")}>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  className="tree-icon-btn"
                  tabIndex={actionsVisible ? 0 : -1}
                  onClick={openMenu}
                >
                  <IconMore size={13} />
                </Button>
              </Tip>
            )}
          </span>
        ) : null}
      </span>
    </div>
  );
}

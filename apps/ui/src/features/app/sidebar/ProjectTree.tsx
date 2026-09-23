import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import type { Project, SessionRow } from "@/features/app/models";
import { isProjectPathMissing } from "@/lib/projectPath";
import { moveId, type SidebarSortMode } from "@/lib/sidebarOrder";
import {
  SIDEBAR_SESSION_ROW_GAP,
  SIDEBAR_SESSION_ROW_HEIGHT,
  SIDEBAR_TOUCH_SESSION_ROW_HEIGHT,
} from "@/lib/virtualList";
import { Button } from "@appica/ui-react/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@appica/ui-react/collapsible";
import { Badge } from "@appica/ui-react/badge";
import { Tip } from "@/components/ui/tooltip";
import { VirtualList } from "@/components/VirtualList";
import {
  IconArrowsVerticalCollapse,
  IconChevronDown,
  IconFolder,
  IconFolderOpen,
  IconMore,
  IconPlus,
  IconNewChat as IconSquarePen,
} from "@/components/icons";
import { SidebarSessionRow } from "./SidebarSessionRow";
import { SidebarSortMenu } from "./SidebarSortMenu";
import type {
  SidebarAddProject,
  SidebarApplyProjectOrder,
  SidebarDragOverProject,
  SidebarDropProject,
  SidebarNewChat,
  SidebarOpenProjectMenu,
  SidebarRelocateProject,
  SidebarSessionActions,
  SidebarSessionStatus,
  SidebarSetState,
  SidebarShowToast,
  SidebarTranslator,
  SidebarProjectDropHint,
} from "./types";

export interface ProjectTreeProps
  extends SidebarSessionActions,
    SidebarSessionStatus {
  tr: SidebarTranslator;
  projects: Project[];
  /** Web Host 只允许选择和展开 Session cwd 项目投影。 */
  canWriteProjects: boolean;
  projectsOpen: boolean;
  setProjectsOpen: SidebarSetState<boolean>;
  toggleProject: (project: Project) => Promise<void>;
  expandedProjects: Record<string, boolean>;
  setExpandedProjects: SidebarSetState<Record<string, boolean>>;
  projectDropHint: SidebarProjectDropHint | null;
  dragOverProject: SidebarDragOverProject;
  dropProject: SidebarDropProject;
  setProjectDropHint: SidebarSetState<SidebarProjectDropHint | null>;
  sessionsForProject: (projectId: string) => SessionRow[];
  visibleSessionsByProject: Record<string, number>;
  setVisibleSessionsByProject: SidebarSetState<Record<string, number>>;
  newChat: SidebarNewChat;
  openProjectMenu: SidebarOpenProjectMenu;
  relocateProject: SidebarRelocateProject;
  applyProjectOrder: SidebarApplyProjectOrder;
  addProject: SidebarAddProject;
  showToast: SidebarShowToast;
  /** 会话排序方式；项目、置顶与独立会话共用。 */
  sessionSortMode: SidebarSortMode;
  onSessionSortModeChange: (mode: SidebarSortMode) => void;
}

function moveProjectWithKeyboard(
  event: ReactKeyboardEvent<HTMLElement>,
  project: Project,
  projects: Project[],
  applyProjectOrder: SidebarApplyProjectOrder,
  showToast: SidebarShowToast,
  tr: SidebarTranslator,
) {
  if (
    !event.altKey ||
    (event.key !== "ArrowUp" && event.key !== "ArrowDown")
  ) {
    return false;
  }
  event.preventDefault();
  const index = projects.findIndex((candidate) => candidate.id === project.id);
  const moveDown = event.key === "ArrowDown";
  const target = projects[index + (moveDown ? 1 : -1)];
  if (target) {
    const ids = moveId(
      projects.map(({ id }) => id),
      project.id,
      target.id,
      moveDown,
    );
    applyProjectOrder(ids);
    showToast(
      tr("sidebar.projectMoved", {
        name: project.name,
        position: ids.indexOf(project.id) + 1,
        total: ids.length,
      }),
    );
  }
  return true;
}

export function ProjectTree({
  tr,
  projects,
  canWriteProjects,
  projectsOpen,
  setProjectsOpen,
  expandedProjects,
  toggleProject,
  setExpandedProjects,
  projectDropHint,
  startSidebarDrag,
  endSidebarDrag,
  dragOverProject,
  dropProject,
  setProjectDropHint,
  sessionsForProject,
  visibleSessionsByProject,
  setVisibleSessionsByProject,
  newChat,
  dropSession,
  session,
  busyIds,
  unreadTerminalResults,
  pendingAskUserSessionIds,
  openProjectMenu,
  relocateProject,
  openSession,
  openSessionMenu,
  archiveSession,
  pinSession,
  applyProjectOrder,
  addProject,
  showToast,
  sessionSortMode,
  onSessionSortModeChange,
}: ProjectTreeProps) {
  return (
    <Collapsible open={projectsOpen} onOpenChange={setProjectsOpen} className="contents">
      <div className="tree-l1 tree-l1--top-spaced">
        <CollapsibleTrigger
          render={
            <Button
              type="button"
              variant="ghost"
              size="md"
              className="tree-l1__head"
            />
          }
        >
          <span className="tree-l1__label">{tr("sidebar.projects")}</span>
          <IconChevronDown size={14} className="chevron--disclose" />
        </CollapsibleTrigger>
        <div className="tree-l1__actions">
          <SidebarSortMenu
            tr={tr}
            mode={sessionSortMode}
            onModeChange={onSessionSortModeChange}
          />
          {projects.length > 0 ? (
            <Tip label={tr("sidebar.collapseAllProjects")}>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                className="tree-l1__action"
                aria-label={tr("sidebar.collapseAllProjects")}
                onClick={(event) => {
                  event.stopPropagation();
                  setExpandedProjects((previous) => {
                    const next = { ...previous };
                    for (const project of projects) next[project.id] = false;
                    return next;
                  });
                }}
              >
                <IconArrowsVerticalCollapse size={15} />
              </Button>
            </Tip>
          ) : null}
          {canWriteProjects ? (
            <Tip label={tr("sidebar.addProject")}>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                className="tree-l1__action"
                aria-label={tr("sidebar.addProject")}
                onClick={() => void addProject()}
              >
                <IconPlus size={15} />
              </Button>
            </Tip>
          ) : null}
        </div>
      </div>

      <CollapsibleContent>
      {projects.length === 0 ? (
        <div className="sidebar-empty">{tr("sidebar.noProjects")}</div>
      ) : null}

      {projects.map((project) => {
            const open = expandedProjects[project.id] === true;
            const projectSessions = sessionsForProject(project.id);
            const visibleSessionCount =
              visibleSessionsByProject[project.id] ?? 20;
            const visibleSessions = projectSessions.slice(
              0,
              visibleSessionCount,
            );
            const pathMissing = isProjectPathMissing(project.pathOk);

            return (
              <div key={project.id} className="tree-project">
                {/* This draggable row owns nested action buttons, so Button would create invalid nesting. */}
                <div
                  draggable={canWriteProjects}
                  onDragStart={canWriteProjects
                    ? (event) => startSidebarDrag(event, "project", project.id)
                    : undefined}
                  onDragEnd={canWriteProjects ? endSidebarDrag : undefined}
                  onDragOver={canWriteProjects
                    ? (event) => dragOverProject(event, project.id)
                    : undefined}
                  onDragLeave={canWriteProjects
                    ? (event) => {
                        if (
                          !event.currentTarget.contains(
                            event.relatedTarget as Node | null,
                          )
                        ) {
                          setProjectDropHint(null);
                        }
                      }
                    : undefined}
                  onDrop={canWriteProjects
                    ? (event) => dropProject(event, project.id)
                    : undefined}
                  className={
                    "tree-l2" +
                    (canWriteProjects && projectDropHint?.id === project.id
                      ? projectDropHint.after
                        ? " tree-l2--drop-after"
                        : " tree-l2--drop-before"
                      : "") +
                    (pathMissing ? " tree-l2--path-missing" : "")
                  }
                  role="button"
                  tabIndex={0}
                  aria-expanded={open}
                  aria-keyshortcuts={canWriteProjects ? "Alt+ArrowUp Alt+ArrowDown" : undefined}
                  onClick={() => void toggleProject(project)}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    if (canWriteProjects) openProjectMenu(event, project);
                  }}
                  onKeyDown={(event) => {
                    if (
                      canWriteProjects && moveProjectWithKeyboard(
                        event,
                        project,
                        projects,
                        applyProjectOrder,
                        showToast,
                        tr,
                      )
                    ) {
                      return;
                    }
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      void toggleProject(project);
                    }
                  }}
                >
                  <span className="tree-l2__icon">
                    {open ? (
                      <IconFolderOpen size={17} />
                    ) : (
                      <IconFolder size={17} />
                    )}
                  </span>
                  <Tip
                    label={
                      pathMissing
                        ? tr("project.pathMissing", { name: project.name })
                        : project.path
                    }
                  >
                    <span className="tree-l2__name">{project.name}</span>
                  </Tip>
                  {pathMissing ? (
                    <Badge size="md" variant="error">
                      {tr("sidebar.pathMissing")}
                    </Badge>
                  ) : null}
                  <span className="tree-l2__actions">
                    <Tip label={tr("sidebar.newConversation")}>
                      <Button
                        type="button"
                        variant="ghost" size="icon-sm" className="tree-icon-btn"
                        disabled={pathMissing}
                        onClick={(event) => {
                          event.stopPropagation();
                          void newChat(project);
                        }}
                      >
                        <IconSquarePen size={14} />
                      </Button>
                    </Tip>
                    {canWriteProjects ? (
                      <Tip label={tr("sidebar.menu")}>
                        <Button
                          type="button"
                          variant="ghost" size="icon-sm" className="tree-icon-btn"
                          onClick={(event) => openProjectMenu(event, project)}
                        >
                          <IconMore size={14} />
                        </Button>
                      </Tip>
                    ) : null}
                  </span>
                </div>

                {open ? (
                  <div className="tree-l3-list-wrap">
                    {canWriteProjects && pathMissing ? (
                      <Button size="md"
                        type="button"
                        variant="ghost"
                        className="tree-l3 tree-l3--hint"
                        onClick={(event) => {
                          event.stopPropagation();
                          void relocateProject(project);
                        }}
                      >
                        {tr("sidebar.relocateProject")}
                      </Button>
                    ) : null}
                    {projectSessions.length > 0 ? (
                      <VirtualList
                        className="tree-l3-list"
                        items={visibleSessions}
                        getKey={(item) => item.id}
                        rowHeight={SIDEBAR_SESSION_ROW_HEIGHT}
                        touchRowHeight={SIDEBAR_TOUCH_SESSION_ROW_HEIGHT}
                        gap={SIDEBAR_SESSION_ROW_GAP}
                        scrollToKey={
                          session.sessionId &&
                          visibleSessions.some(
                            (item) => item.id === session.sessionId,
                          )
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
                            project={project}
                            activeSessionId={session.sessionId}
                            working={busyIds.has(item.id)}
                            loading={session.sessionId === item.id && session.state === "connecting"}
                            unreadResult={unreadTerminalResults.get(item.id) ?? null}
                            needsInput={pendingAskUserSessionIds.has(item.id)}
                            variant="project"
                          />
                        )}
                      />
                    ) : null}
                    {projectSessions.length > visibleSessionCount ? (
                      <Button
                        type="button"
                        variant="ghost"
                        size="md"
                        className="tree-l3-more"
                        onClick={() =>
                          setVisibleSessionsByProject((counts) => ({
                            ...counts,
                            [project.id]: visibleSessionCount + 20,
                          }))
                        }
                      >
                        {tr("sidebar.showMore")}
                      </Button>
                    ) : null}
                    {projectSessions.length === 0 ? (
                      <div
                        className="sidebar-empty sidebar-empty--compact"
                      >
                        {tr("sidebar.noChats")}
                      </div>
                    ) : null}
                  </div>
                ) : null}
              </div>
            );
          })}
      </CollapsibleContent>
    </Collapsible>
  );
}

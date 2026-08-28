import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import type { Project, SessionRow } from "@/features/app/models";
import { isProjectPathMissing } from "@/lib/projectPath";
import { moveId } from "@/lib/sidebarOrder";
import { SIDEBAR_SESSION_ROW_GAP, SIDEBAR_SESSION_ROW_HEIGHT } from "@/lib/virtualList";
import { Button } from "@/components/ui/button";
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
  projectsOpen: boolean;
  setProjectsOpen: SidebarSetState<boolean>;
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
}

function toggleProject(
  setExpandedProjects: SidebarSetState<Record<string, boolean>>,
  projectId: string,
  open: boolean,
) {
  setExpandedProjects((expanded) => ({ ...expanded, [projectId]: !open }));
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
  projectsOpen,
  setProjectsOpen,
  expandedProjects,
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
  completedUnreadIds,
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
}: ProjectTreeProps) {
  return (
    <>
      <div className="tree-l1" style={{ marginTop: 8 }}>
        <Button
          type="button"
          className="tree-l1__head"
          onClick={() => setProjectsOpen((value) => !value)}
          aria-expanded={projectsOpen}
        >
          <span className="tree-l1__label">{tr("sidebar.projects")}</span>
          <IconChevronDown size={14} className="chevron--disclose" />
        </Button>
        <div className="tree-l1__actions">
          {projects.length > 0 ? (
            <Tip label={tr("sidebar.collapseAllProjects")}>
              <Button
                type="button"
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
          <Tip label={tr("sidebar.addProject")}>
            <Button
              type="button"
              className="tree-l1__action"
              aria-label={tr("sidebar.addProject")}
              onClick={() => void addProject()}
            >
              <IconPlus size={15} />
            </Button>
          </Tip>
        </div>
      </div>

      {projectsOpen && projects.length === 0 ? (
        <div className="sidebar-empty">{tr("sidebar.noProjects")}</div>
      ) : null}

      {projectsOpen
        ? projects.map((project) => {
            const open = expandedProjects[project.id] !== false;
            const projectSessions = sessionsForProject(project.id);
            const visibleSessionCount =
              visibleSessionsByProject[project.id] ?? 5;
            const visibleSessions = projectSessions.slice(
              0,
              visibleSessionCount,
            );
            const pathMissing = isProjectPathMissing(project.pathOk);

            return (
              <div key={project.id} className="tree-project">
                <div
                  draggable
                  onDragStart={(event) =>
                    startSidebarDrag(event, "project", project.id)
                  }
                  onDragEnd={endSidebarDrag}
                  onDragOver={(event) => dragOverProject(event, project.id)}
                  onDragLeave={(event) => {
                    if (
                      !event.currentTarget.contains(
                        event.relatedTarget as Node | null,
                      )
                    ) {
                      setProjectDropHint(null);
                    }
                  }}
                  onDrop={(event) => dropProject(event, project.id)}
                  className={
                    "tree-l2" +
                    (projectDropHint?.id === project.id
                      ? projectDropHint.after
                        ? " tree-l2--drop-after"
                        : " tree-l2--drop-before"
                      : "") +
                    (pathMissing ? " tree-l2--path-missing" : "")
                  }
                  role="button"
                  tabIndex={0}
                  aria-expanded={open}
                  aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown"
                  onClick={() => toggleProject(setExpandedProjects, project.id, open)}
                  onContextMenu={(event) => openProjectMenu(event, project)}
                  onKeyDown={(event) => {
                    if (
                      moveProjectWithKeyboard(
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
                      toggleProject(setExpandedProjects, project.id, open);
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
                    <span className="project-row__badge project-row__badge--path-missing">
                      {tr("sidebar.pathMissing")}
                    </span>
                  ) : null}
                  <span className="tree-l2__actions">
                    <Tip label={tr("sidebar.newConversation")}>
                      <Button
                        type="button"
                        className="tree-icon-btn"
                        disabled={pathMissing}
                        onClick={(event) => {
                          event.stopPropagation();
                          void newChat(project);
                        }}
                      >
                        <IconSquarePen size={14} />
                      </Button>
                    </Tip>
                    <Tip label={tr("sidebar.menu")}>
                      <Button
                        type="button"
                        className="tree-icon-btn"
                        onClick={(event) => openProjectMenu(event, project)}
                      >
                        <IconMore size={14} />
                      </Button>
                    </Tip>
                  </span>
                </div>

                {open ? (
                  <div className="tree-l3-list-wrap">
                    {pathMissing ? (
                      <Button
                        type="button"
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
                            completedUnread={completedUnreadIds.has(item.id)}
                            needsInput={pendingAskUserSessionIds.has(item.id)}
                            variant="project"
                          />
                        )}
                      />
                    ) : null}
                    {projectSessions.length > visibleSessionCount ? (
                      <Button
                        type="button"
                        className="tree-l3-more"
                        onClick={() =>
                          setVisibleSessionsByProject((counts) => ({
                            ...counts,
                            [project.id]: visibleSessionCount + 5,
                          }))
                        }
                      >
                        {tr("sidebar.showMore")}
                      </Button>
                    ) : null}
                    {projectSessions.length === 0 ? (
                      <div
                        className="sidebar-empty"
                        style={{ padding: "4px 10px" }}
                      >
                        {tr("sidebar.noChats")}
                      </div>
                    ) : null}
                  </div>
                ) : null}
              </div>
            );
          })
        : null}
    </>
  );
}

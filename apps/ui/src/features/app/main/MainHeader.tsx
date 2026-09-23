import type {
  Dispatch,
  MouseEvent as ReactMouseEvent,
  RefObject,
  SetStateAction,
} from "react";
import { useState } from "react";
import type { MessageKey, Vars } from "@/i18n";
import type { LayoutPrefs } from "@/lib/layout";
import type { Project, SessionRow } from "@/features/app/models";
import type { SessionSnapshot } from "@/lib/session";
import type { GitWorktreeEntry } from "@/lib/api";
import { Button } from "@appica/ui-react/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuGroupLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tip } from "@/components/ui/tooltip";
import {
  IconArrowLeft,
  IconArrowRight,
  IconGitBranch,
  IconMore,
  IconNewChat,
  IconPanel,
  IconPanelRight,
  IconFolder,
  IconSummary,
} from "@/components/icons";
import { saveLayout } from "@/lib/layout";
import { isPlaceholderSessionTitle } from "@/lib/sessionTitle";
import { pathsEqual } from "@/lib/gitWorktree";

type SetState<T> = Dispatch<SetStateAction<T>>;
type Translator = (key: MessageKey, vars?: Vars) => string;
type NewChat = (
  project?: Project | null,
  options?: { seedDraft?: string },
) => void | Promise<void>;
type BindSessionProject = (
  project: Project | null,
  options?: { silent?: boolean },
) => Promise<void>;

/** 从已经加载的 worktree 列表中解析当前项目的分支；不额外触发 Git 查询。 */
export function resolveHeaderWorktreeBranch(
  projectPath: string | null | undefined,
  worktrees: GitWorktreeEntry[],
  detachedLabel: string,
): string | null {
  const path = projectPath?.trim();
  if (!path) return null;
  const current = worktrees.find((worktree) => pathsEqual(worktree.path, path));
  if (!current) return null;
  const branch = current.branch?.trim();
  if (branch) return branch;
  return current.detached ? detachedLabel : null;
}

export interface MainHeaderProps {
  layout: LayoutPrefs;
  setLayout: SetState<LayoutPrefs>;
  useCustomWindowChrome: boolean;
  toggleMaximizeFromTitlebar: () => Promise<void>;
  tr: Translator;
  sessions: SessionRow[];
  activeProject: Project | null;
  projects: Project[];
  gitWorktrees: GitWorktreeEntry[];
  bindSessionProject: BindSessionProject;
  session: SessionSnapshot;
  summaryOpen: boolean;
  summaryTriggerRef: RefObject<HTMLButtonElement | null>;
  setSummaryOpen: SetState<boolean>;
  openSessionMenu: (event: ReactMouseEvent, session: SessionRow) => void;
  newChat: NewChat;
  canGoBack: boolean;
  canGoForward: boolean;
  goBack: () => Promise<void>;
  goForward: () => Promise<void>;
}

export function MainHeader({
  layout,
  setLayout,
  useCustomWindowChrome,
  toggleMaximizeFromTitlebar,
  tr,
  sessions,
  activeProject,
  projects,
  gitWorktrees,
  bindSessionProject,
  session,
  summaryOpen,
  summaryTriggerRef,
  setSummaryOpen,
  openSessionMenu,
  newChat,
  canGoBack,
  canGoForward,
  goBack,
  goForward,
}: MainHeaderProps) {
  const current = sessions.find((item) => item.id === session.sessionId);
  const title = current?.title || session.title || "";
  const showTitle = !isPlaceholderSessionTitle(title, [
    tr("session.new"),
    tr("session.placeholderTitle"),
  ]);
  const [projectMenuOpen, setProjectMenuOpen] = useState(false);
  const currentBranch = resolveHeaderWorktreeBranch(
    activeProject?.path,
    gitWorktrees,
    tr("composer.worktreeDetached"),
  );
  const projectContextLabel = [activeProject?.name, currentBranch]
    .filter(Boolean)
    .join(" · ");
  const projectContextTooltip = [activeProject?.path, currentBranch]
    .filter(Boolean)
    .join("\n");

  return (
    <div
      className="main__top"
      data-tauri-drag-region
      onDoubleClick={() => {
        if (useCustomWindowChrome) void toggleMaximizeFromTitlebar();
      }}
    >
      <div className="main__title-row">
        {layout.sidebarCollapsed && (
          <>
            <Tip label={tr("main.leftPaneShow")}>
              <Button
                type="button"
                variant="ghost"
                size="md"
                className="main__sidebar-toggle"
                aria-label={tr("main.leftPaneShow")}
                onClick={() =>
                  setLayout((currentLayout) => {
                    const next = { ...currentLayout, sidebarCollapsed: false };
                    saveLayout(localStorage, next);
                    return next;
                  })
                }
              >
                <IconPanel size={16} />
              </Button>
            </Tip>
            <div className="main__task-nav" data-testid="main-task-navigation">
              <Tip label={tr("resources.browserBack")}>
                <Button
                  type="button"
                  variant="ghost"
                  size="md"
                  className="main__task-nav-button"
                  aria-label={tr("resources.browserBack")}
                  disabled={!canGoBack}
                  onClick={() => void goBack()}
                >
                  <IconArrowLeft size={16} />
                </Button>
              </Tip>
              <Tip label={tr("resources.browserForward")}>
                <Button
                  type="button"
                  variant="ghost"
                  size="md"
                  className="main__task-nav-button"
                  aria-label={tr("resources.browserForward")}
                  disabled={!canGoForward}
                  onClick={() => void goForward()}
                >
                  <IconArrowRight size={16} />
                </Button>
              </Tip>
            </div>
            <Tip label={tr("sidebar.newSession")}>
              <Button
                type="button"
                variant="ghost"
                size="md"
                className="main__new-task-button"
                aria-label={tr("sidebar.newSession")}
                onClick={() => void newChat(null)}
              >
                <IconNewChat size={16} />
              </Button>
            </Tip>
          </>
        )}
        {activeProject ? (
          <DropdownMenu
            open={projectMenuOpen}
            onOpenChange={setProjectMenuOpen}
            size="md"
          >
            <Tip label={projectContextTooltip} disabled={projectMenuOpen}>
              <DropdownMenuTrigger
                data-testid="main-project-context"
                className="main__project-context"
                aria-label={projectContextLabel || activeProject.path}
                render={
                  <Button type="button" variant="ghost" size="md" />
                }
              >
                <IconFolder size={16} />
              </DropdownMenuTrigger>
            </Tip>
            <DropdownMenuContent
              className="main__project-context-popover"
              align="start"
              sideOffset={6}
            >
              <div className="main__project-context-summary">
                <div className="main__project-context-name">
                  <IconFolder size={16} />
                  <strong title={activeProject.name}>{activeProject.name}</strong>
                </div>
                <code
                  className="main__project-context-path"
                  title={activeProject.path}
                >
                  {activeProject.path}
                </code>
                {currentBranch ? (
                  <div className="main__project-context-branch">
                    <IconGitBranch size={15} />
                    <span title={currentBranch}>{currentBranch}</span>
                  </div>
                ) : null}
                {activeProject.pathOk === false ? (
                  <span className="main__project-context-missing">
                    {tr("project.pathMissingShort")}
                  </span>
                ) : null}
              </div>
              {projects.length > 1 ? (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuGroup>
                    <DropdownMenuGroupLabel>
                      {tr("composer.pickProject")}
                    </DropdownMenuGroupLabel>
                    <DropdownMenuRadioGroup
                      value={activeProject.id}
                      onValueChange={(projectId) => {
                        const project = projects.find(
                          (candidate) => candidate.id === projectId,
                        );
                        if (project) void bindSessionProject(project);
                      }}
                    >
                      {projects.map((project) => (
                        <DropdownMenuRadioItem
                          key={project.id}
                          value={project.id}
                          className="main__project-context-option"
                          title={project.path}
                        >
                          <span className="main__project-context-option-copy">
                            <span className="main__project-context-option-name">
                              {project.name}
                            </span>
                            <span className="main__project-context-option-path">
                              {project.path}
                            </span>
                          </span>
                        </DropdownMenuRadioItem>
                      ))}
                    </DropdownMenuRadioGroup>
                  </DropdownMenuGroup>
                </>
              ) : null}
            </DropdownMenuContent>
          </DropdownMenu>
        ) : null}
        {showTitle ? (
          <>
            <Tip label={title}>
              <h1 className="main__title">
                {title}
              </h1>
            </Tip>
            {current && (
              <Tip label={tr("session.menu")}>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-md"
                  className="main__title-menu"
                  onClick={(event) => openSessionMenu(event, current)}
                >
                  <IconMore size={16} />
                </Button>
              </Tip>
            )}
          </>
        ) : null}
      </div>

      {session.sessionId ? (
        <div className="main__top-actions">
          <Tip
            label={
              summaryOpen
                ? tr("main.summaryHide")
                : tr("main.summaryShow")
            }
          >
            <Button
              ref={summaryTriggerRef}
              type="button"
              variant={summaryOpen ? "soft" : "ghost"}
              size="icon-md"
              className={
                "chrome-btn main__pane-toggle" +
                (summaryOpen ? " is-on" : "")
              }
              aria-pressed={summaryOpen}
              onClick={() => setSummaryOpen((value) => !value)}
            >
              <IconSummary size={16} />
            </Button>
          </Tip>
          <Tip
            label={
              layout.asideCollapsed
                ? tr("main.rightPaneShow")
                : tr("main.rightPaneHide")
            }
          >
            <Button
              type="button"
              variant={!layout.asideCollapsed ? "soft" : "ghost"}
              size="icon-md"
              className={
                "chrome-btn main__pane-toggle" +
                (!layout.asideCollapsed ? " is-on" : "")
              }
              onClick={() =>
                setLayout((currentLayout) => {
                  const next = {
                    ...currentLayout,
                    asideCollapsed: !currentLayout.asideCollapsed,
                  };
                  saveLayout(localStorage, next);
                  return next;
                })
              }
            >
              <IconPanelRight size={16} />
            </Button>
          </Tip>
        </div>
      ) : null}
    </div>
  );
}

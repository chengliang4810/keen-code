import {
  useEffect,
  useMemo,
  useState,
  type ReactNode,
  type PointerEvent,
} from "react";
import { createPortal } from "react-dom";
import { Button } from "@/components/ui/button";
import { SidebarPrimaryAction } from "@/modules/sidebar/SidebarPrimaryAction";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from "@/components/ui/hover-card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSub,
  DropdownMenuSubTrigger,
  DropdownMenuSubContent,
} from "@/components/ui/dropdown-menu";
import { Spinner } from "@/components/localized/spinner";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Add01Icon,
  ArrowDown01Icon,
  ArrowRight01Icon,
  ArrowUpDownIcon,
  BubbleChatIcon,
  Folder01Icon,
  FolderOpenIcon,
  PencilEdit02Icon,
  MoreHorizontalIcon,
  PinIcon,
  Settings01Icon,
} from "@hugeicons/core-free-icons";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/modules/i18n";
import { useSpaces } from "@/modules/spaces";
import { useChatStore } from "@/modules/ai/store/chatStore";
import { StorageLoadError } from "@/modules/ai/components/StorageLoadError";
import { usePlanStore } from "@/modules/ai/store/planStore";
import { isTaskRunning } from "@/modules/ai/lib/taskWorkspace";
import {
  sidebarNavigationGroups,
  type SidebarProjectGroup,
  type SidebarSectionId,
  type ProjectDropEdge,
  type ProjectDropTarget,
  conversationOwner,
} from "@/modules/ai/lib/sidebarNavigation";
import { useProjectReorder } from "@/modules/ai/lib/useProjectReorder";
import { useSidebarNavigation } from "@/modules/ai/store/sidebarNavigation";
import type { SessionMeta } from "@/modules/ai/lib/sessions";

type Props = {
  onNewConversation: () => void;
  onNewProject: () => void;
  onRemoveProject: (projectId: string) => Promise<void>;
  onNewIndependentConversation: () => void;
  onNewTask: (projectId: string) => void;
  onSelectTask: (projectId: string | null, sessionId: string) => void;
};

export function ProjectTasksSidebar({
  onNewConversation,
  onNewProject,
  onRemoveProject,
  onNewIndependentConversation,
  onNewTask,
  onSelectTask,
}: Props) {
  const tr = useTranslation();
  const projects = useSpaces((s) => s.spaces);
  const sessions = useChatStore((s) => s.sessions);
  const activeId = useChatStore((s) => s.activeSessionId);
  const status = useChatStore((s) => s.agentMeta.status);
  const pendingEdits = usePlanStore((s) => s.queue.length);
  const sessionsReady = useChatStore((s) => s.sessionsHydrated);
  const sessionsError = useChatStore((s) => s.sessionsError);
  const sessionsLoading = useChatStore((s) => s.sessionsLoading);
  const hydrateSessions = useChatStore((s) => s.hydrateSessions);
  const projectsReady = useSpaces((s) => s.hydrated);
  const projectsError = useSpaces((s) => s.loadError);
  const projectsLoading = useSpaces((s) => s.loading);
  const retryProjects = useSpaces((s) => s.retryLoad);
  const ready = sessionsReady && projectsReady;
  const locked = isTaskRunning(status) || pendingEdits > 0;
  // 项目折叠只影响列表展示，不切换当前对话；每次启动均从全部收起开始。
  const [openProjects, setOpenProjects] = useState<Set<string>>(
    () => new Set(),
  );
  const [showAllProjects, setShowAllProjects] = useState<Set<string>>(
    () => new Set(),
  );
  const [editingProject, setEditingProject] = useState<string | null>(null);
  const [hoveredProject, setHoveredProject] = useState<string | null>(null);
  const [projectMenu, setProjectMenu] = useState<string | null>(null);
  // 统计项目全部未归档对话，包含单独置顶项，不受列表截断影响。
  const conversationCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const task of sessions) {
      if (!task.projectless && task.projectId && !task.archived)
        counts.set(task.projectId, (counts.get(task.projectId) ?? 0) + 1);
    }
    return counts;
  }, [sessions]);
  const navigation = useSidebarNavigation();
  useEffect(() => {
    void navigation.init();
  }, [navigation.init]);
  const pinDisabled = !navigation.hydrated || navigation.saving;
  const conversationSort = navigation.snapshot.conversationSort ?? "recent";
  const groups = useMemo(
    () =>
      sidebarNavigationGroups(
        projects,
        sessions,
        navigation.snapshot.pins,
        false,
        navigation.snapshot.projectOrder,
        conversationSort,
        navigation.snapshot.conversationOrder,
      ),
    [
      projects,
      sessions,
      navigation.snapshot.pins,
      navigation.snapshot.projectOrder,
      conversationSort,
      navigation.snapshot.conversationOrder,
    ],
  );
  const reorder = useProjectReorder({
    disabled: locked || !ready || pinDisabled || editingProject !== null,
    onMove: (source, target, edge) =>
      source.kind === "task"
        ? void navigation.moveConversation(sessions, source.id, target.id, edge)
        : void navigation.moveProject(
            projects.map((project) => project.id),
            source,
            target,
            edge,
          ),
  });
  // 拖拽时收起信息浮层，避免遮挡落点，释放后也不自动重新弹出。
  useEffect(() => {
    if (reorder.dragging) setHoveredProject(null);
  }, [reorder.dragging]);
  const dropEdge = (
    id: string,
    kind: "project" | "task",
    pinned: boolean,
    section: SidebarSectionId = "projects",
  ) =>
    reorder.drop?.target.id === id &&
    reorder.drop.target.kind === kind &&
    reorder.drop.target.section === (pinned ? "pinned" : section)
      ? reorder.drop.edge
      : undefined;
  const moveWithKeyboard = (
    source: ProjectDropTarget,
    direction: "up" | "down",
  ) => {
    if (locked || !ready || pinDisabled) return;
    const targets: ProjectDropTarget[] =
      source.section === "pinned"
        ? groups.pinned.map((item) => ({
            section: "pinned",
            kind: item.kind,
            id: item.kind === "project" ? item.group.project.id : item.task.id,
          }))
        : groups.projects.map((group) => ({
            section: "projects",
            kind: "project",
            id: group.project.id,
          }));
    const index = targets.findIndex(
      (target) => target.id === source.id && target.kind === source.kind,
    );
    const target = targets[index + (direction === "up" ? -1 : 1)];
    if (target)
      void navigation.moveProject(
        projects.map((project) => project.id),
        source,
        target,
        direction === "up" ? "before" : "after",
      );
  };
  const moveConversationWithKeyboard = (
    task: SessionMeta,
    direction: "up" | "down",
  ) => {
    if (conversationSort !== "manual" || locked || !ready || pinDisabled)
      return;
    const projectGroups = [
      ...groups.projects,
      ...groups.pinned.flatMap((item) =>
        item.kind === "project" ? [item.group] : [],
      ),
    ];
    const tasks = task.projectless
      ? groups.conversations
      : (projectGroups.find((group) => group.project.id === task.projectId)
          ?.tasks ?? []);
    const index = tasks.findIndex((candidate) => candidate.id === task.id);
    const target = tasks[index + (direction === "up" ? -1 : 1)];
    if (target)
      void navigation.moveConversation(
        sessions,
        task.id,
        target.id,
        direction === "up" ? "before" : "after",
      );
  };
  const renderTask = (task: SessionMeta, topLevel = false, pinned = false) => (
    <TaskRow
      key={`task:${task.id}`}
      task={task}
      active={task.id === activeId}
      locked={locked}
      status={task.id === activeId ? status : "idle"}
      topLevel={topLevel}
      pinned={pinned}
      pinDisabled={pinDisabled}
      dropEdge={dropEdge(
        task.id,
        "task",
        pinned,
        task.projectless ? "conversations" : "projects",
      )}
      dragging={
        reorder.dragging?.source.kind === "task" &&
        reorder.dragging.source.id === task.id
      }
      onPointerDown={
        conversationSort === "manual" && !pinned
          ? (event) =>
              reorder.onPointerDown(
                event,
                {
                  kind: "task",
                  id: task.id,
                  section: task.projectless ? "conversations" : "projects",
                  owner: conversationOwner(task),
                },
                task.title === "New chat" ? tr("New task") : task.title,
              )
          : undefined
      }
      onMoveWithKeyboard={
        conversationSort === "manual" && !pinned
          ? (direction) => moveConversationWithKeyboard(task, direction)
          : undefined
      }
      onTogglePin={() =>
        void navigation.togglePin({ kind: "task", id: task.id })
      }
      onSelect={() =>
        onSelectTask(
          task.projectless ? null : (task.projectId ?? null),
          task.id,
        )
      }
    />
  );

  // 对话是主要阅读入口，项目名和栏目说明依次降低字号与对比度。
  const renderProject = (
    { project, tasks }: SidebarProjectGroup,
    pinned = false,
  ) => (
    <section
      key={`project:${project.id}`}
      data-project-id={project.id}
      data-sidebar-drop-id={project.id}
      data-sidebar-drop-kind="project"
      data-sidebar-drop-section={pinned ? "pinned" : "projects"}
      className={cn(
        "relative",
        reorder.dragging?.source.kind === "project" &&
          reorder.dragging.source.id === project.id &&
          "opacity-50",
      )}
    >
      <ProjectDropLine edge={dropEdge(project.id, "project", pinned)} />
      <HoverCard
        openDelay={450}
        closeDelay={150}
        open={
          hoveredProject === project.id &&
          !reorder.dragging &&
          editingProject !== project.id &&
          projectMenu !== project.id
        }
        onOpenChange={(open) =>
          setHoveredProject((current) =>
            open &&
            !reorder.dragging &&
            projectMenu !== project.id &&
            editingProject !== project.id
              ? project.id
              : current === project.id
                ? null
                : current,
          )
        }
      >
        <HoverCardTrigger asChild>
          <div className="group flex h-9 items-center gap-2 rounded-md px-2 hover:bg-foreground/[0.045] focus-within:bg-foreground/[0.045]">
            {editingProject === project.id ? (
              <>
                <HugeiconsIcon
                  icon={
                    openProjects.has(project.id) ? FolderOpenIcon : Folder01Icon
                  }
                  size={14}
                  className="shrink-0 text-muted-foreground"
                  aria-hidden="true"
                />
                <RenameInput
                  label={tr("Rename project")}
                  initial={project.name}
                  className="text-ui-base text-foreground/75 md:text-ui-base"
                  onCommit={(value) => {
                    if (value) useSpaces.getState().rename(project.id, value);
                    setEditingProject(null);
                  }}
                />
              </>
            ) : (
              <button
                type="button"
                onPointerDown={(event) =>
                  reorder.onPointerDown(
                    event,
                    {
                      kind: "project",
                      id: project.id,
                      section: pinned ? "pinned" : "projects",
                    },
                    project.name,
                  )
                }
                onKeyDown={(event) => {
                  if (
                    event.altKey &&
                    (event.key === "ArrowUp" || event.key === "ArrowDown")
                  ) {
                    event.preventDefault();
                    moveWithKeyboard(
                      {
                        kind: "project",
                        id: project.id,
                        section: pinned ? "pinned" : "projects",
                      },
                      event.key === "ArrowUp" ? "up" : "down",
                    );
                  }
                }}
                aria-expanded={openProjects.has(project.id)}
                aria-controls={`project-tasks-${project.id}`}
                onClick={() =>
                  setOpenProjects((current) => {
                    const next = new Set(current);
                    if (next.has(project.id)) next.delete(project.id);
                    else next.add(project.id);
                    return next;
                  })
                }
                aria-description={tr(
                  "Drag to reorder. Alt + Up/Down also moves the project.",
                )}
                className="flex min-w-0 flex-1 touch-none select-none items-center gap-2 rounded-sm py-1 text-left text-ui-base font-normal text-foreground/75 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <HugeiconsIcon
                  icon={
                    openProjects.has(project.id) ? FolderOpenIcon : Folder01Icon
                  }
                  size={14}
                  className="shrink-0 text-muted-foreground"
                  aria-hidden="true"
                />
                <span className="min-w-0 flex-1 truncate">{project.name}</span>
              </button>
            )}
            <Button
              size="icon-sm"
              variant="ghost"
              className="size-6 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 [@media(hover:none)]:opacity-100"
              disabled={locked || !ready}
              onClick={() => onNewTask(project.id)}
              aria-label={tr("New task in {project}", {
                project: project.name,
              })}
              title={tr("New task")}
            >
              <HugeiconsIcon icon={Add01Icon} size={12} />
            </Button>
            <DropdownMenu
              open={projectMenu === project.id}
              onOpenChange={(open) => {
                setProjectMenu(open ? project.id : null);
                if (open) setHoveredProject(null);
              }}
            >
              <DropdownMenuTrigger asChild>
                <Button
                  size="icon-sm"
                  variant="ghost"
                  className="size-6 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 [@media(hover:none)]:opacity-100"
                  aria-label={tr("Project actions")}
                  disabled={locked || !ready}
                >
                  <HugeiconsIcon icon={MoreHorizontalIcon} size={12} />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem
                  disabled={pinDisabled}
                  onSelect={() =>
                    void navigation.togglePin({
                      kind: "project",
                      id: project.id,
                    })
                  }
                >
                  {tr(pinned ? "Unpin" : "Pin project")}
                </DropdownMenuItem>
                <DropdownMenuItem
                  onSelect={() => setEditingProject(project.id)}
                >
                  {tr("Rename project")}
                </DropdownMenuItem>
                <DropdownMenuItem
                  onSelect={() => void onRemoveProject(project.id)}
                >
                  {tr("Remove project")}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </HoverCardTrigger>
        <HoverCardContent
          side="right"
          align="start"
          sideOffset={8}
          collisionPadding={12}
          className="w-80 max-w-[calc(100vw-24px)] max-h-[var(--radix-hover-card-content-available-height)] overflow-y-auto rounded-lg border border-border/70 p-2 text-ui-base shadow-md ring-0"
          aria-label={tr("Project information")}
        >
          <div className="flex items-center gap-2 px-1 py-1">
            <HugeiconsIcon
              icon={Folder01Icon}
              size={14}
              className="shrink-0 text-muted-foreground"
              aria-hidden="true"
            />
            <span
              className="min-w-0 flex-1 truncate font-medium"
              title={project.name}
            >
              {project.name}
            </span>
            <Button
              variant="ghost"
              size="icon-sm"
              className={cn(
                "size-6 rounded-md text-muted-foreground",
                pinned && "text-foreground",
              )}
              disabled={locked || !ready || pinDisabled}
              aria-label={tr(pinned ? "Unpin" : "Pin project")}
              aria-pressed={pinned}
              onClick={() => {
                setHoveredProject(null);
                void navigation.togglePin({ kind: "project", id: project.id });
              }}
            >
              <HugeiconsIcon icon={PinIcon} size={13} />
            </Button>
          </div>
          <div className="flex items-center gap-2 px-1 pb-2 text-ui-sm text-muted-foreground">
            <HugeiconsIcon
              icon={BubbleChatIcon}
              size={14}
              className="shrink-0"
              aria-hidden="true"
            />
            <span>
              {ready
                ? tr("{count} conversations", {
                    count: conversationCounts.get(project.id) ?? 0,
                  })
                : tr("Loading sessions…")}
            </span>
          </div>
          <div className="flex items-center gap-2 border-y border-border/60 px-1 py-2 text-muted-foreground">
            <HugeiconsIcon
              icon={FolderOpenIcon}
              size={14}
              className="shrink-0"
              aria-hidden="true"
            />
            <span
              className="min-w-0 flex-1 truncate"
              dir="auto"
              title={project.root ?? undefined}
            >
              {project.root || tr("No project directory")}
            </span>
          </div>
          <Button
            variant="ghost"
            className="mt-1 h-7 w-full justify-start gap-2 rounded-md px-1 text-ui-base text-muted-foreground"
            disabled={locked || !ready}
            onClick={() => {
              setHoveredProject(null);
              setEditingProject(project.id);
            }}
          >
            <HugeiconsIcon icon={Settings01Icon} size={14} />
            {tr("Rename project")}
          </Button>
        </HoverCardContent>
      </HoverCard>
      <div
        id={`project-tasks-${project.id}`}
        hidden={!openProjects.has(project.id)}
        className="space-y-0.5 pt-0.5"
      >
        {openProjects.has(project.id) && (
          <>
            {tasks.length === 0 ? (
              <p className="py-2 pl-8 pr-2 text-ui-sm text-muted-foreground">
                {tr("No conversations")}
              </p>
            ) : (
              tasks
                .slice(0, showAllProjects.has(project.id) ? tasks.length : 5)
                .map((task) => renderTask(task))
            )}
            {tasks.length > 5 && (
              <button
                type="button"
                className="w-full rounded-md py-1.5 pl-8 pr-2 text-left text-ui-base text-muted-foreground hover:bg-accent"
                onClick={() =>
                  setShowAllProjects((current) => {
                    const next = new Set(current);
                    if (next.has(project.id)) next.delete(project.id);
                    else next.add(project.id);
                    return next;
                  })
                }
              >
                {tr(
                  showAllProjects.has(project.id) ? "Show less" : "Show more",
                )}
              </button>
            )}
          </>
        )}
      </div>
    </section>
  );

  const renderSection = (
    id: SidebarSectionId,
    title: string,
    children: ReactNode,
    action?: ReactNode,
  ) => (
    <SidebarSection
      id={id}
      title={title}
      collapsed={navigation.snapshot.collapsed.includes(id)}
      disabled={pinDisabled}
      onToggle={() => void navigation.toggleSection(id)}
      action={action}
    >
      {children}
    </SidebarSection>
  );

  return (
    <div
      ref={reorder.rootRef}
      onClickCapture={reorder.onClickCapture}
      className="flex h-full min-h-0 flex-col"
      data-project-task-sidebar
    >
      <SidebarPrimaryAction
        onClick={onNewConversation}
        disabled={locked || !ready}
      >
        <HugeiconsIcon icon={PencilEdit02Icon} size={16} strokeWidth={1.75} />
        {tr("New conversation")}
      </SidebarPrimaryAction>
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col gap-3 px-2 pb-3">
          {((!ready && !sessionsError && !projectsError) ||
            (!navigation.hydrated && !navigation.error)) && (
            <div className="flex items-center gap-2 p-2 text-ui-sm text-muted-foreground">
              <Spinner className="size-3" />
              {tr("Loading sessions…")}
            </div>
          )}
          {sessionsError && (
            <StorageLoadError
              error={sessionsError}
              loading={sessionsLoading}
              onRetry={() => {
                void hydrateSessions();
              }}
            />
          )}
          {projectsError && (
            <StorageLoadError
              error={projectsError}
              loading={projectsLoading}
              onRetry={retryProjects}
            />
          )}
          {navigation.error && (
            <div role="alert" className="px-2 text-ui-sm text-destructive">
              <p>
                {tr(
                  navigation.error === "load"
                    ? "Could not restore sidebar navigation."
                    : "Could not save sidebar navigation.",
                )}
              </p>
              <Button
                variant="ghost"
                size="sm"
                disabled={navigation.saving}
                onClick={() => void navigation.retry()}
              >
                {tr("Retry")}
              </Button>
            </div>
          )}
          {groups.pinned.length > 0 &&
            renderSection(
              "pinned",
              tr("Pinned"),
              groups.pinned.map((item) =>
                item.kind === "project"
                  ? renderProject(item.group, true)
                  : renderTask(item.task, true, true),
              ),
            )}
          {renderSection(
            "projects",
            tr("Projects"),
            groups.projects.map((group) => renderProject(group)),
            <div className="pointer-events-none flex items-center gap-1 opacity-0 transition-opacity group-hover/section-header:pointer-events-auto group-hover/section-header:opacity-100 group-focus-within/section-header:pointer-events-auto group-focus-within/section-header:opacity-100 has-[[data-state=open]]:pointer-events-auto has-[[data-state=open]]:opacity-100 [@media(hover:none)]:pointer-events-auto [@media(hover:none)]:opacity-100">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    className="size-6"
                    disabled={pinDisabled}
                    aria-label={tr("Project section actions")}
                    title={tr("Project section actions")}
                  >
                    <HugeiconsIcon icon={MoreHorizontalIcon} size={12} />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  <DropdownMenuSub>
                    <DropdownMenuSubTrigger>
                      <HugeiconsIcon icon={ArrowUpDownIcon} size={16} />
                      {tr("Conversation sorting")}
                    </DropdownMenuSubTrigger>
                    <DropdownMenuSubContent>
                      <DropdownMenuRadioGroup
                        value={conversationSort}
                        onValueChange={(value) => {
                          if (value === "recent" || value === "manual")
                            void navigation.setConversationSort(
                              value,
                              sessions,
                            );
                        }}
                      >
                        <DropdownMenuRadioItem
                          value="recent"
                          disabled={pinDisabled}
                        >
                          {tr("Recently updated")}
                        </DropdownMenuRadioItem>
                        <DropdownMenuRadioItem
                          value="manual"
                          disabled={pinDisabled}
                        >
                          {tr("Manual order")}
                        </DropdownMenuRadioItem>
                      </DropdownMenuRadioGroup>
                    </DropdownMenuSubContent>
                  </DropdownMenuSub>
                </DropdownMenuContent>
              </DropdownMenu>
              <Button
                size="icon-sm"
                variant="ghost"
                className="size-6"
                onClick={onNewProject}
                disabled={locked || !ready}
                title={tr("New project")}
                aria-label={tr("New project")}
              >
                <HugeiconsIcon icon={Add01Icon} size={14} strokeWidth={1.75} />
              </Button>
            </div>,
          )}
          {renderSection(
            "conversations",
            tr("Conversations"),
            groups.conversations.length ? (
              groups.conversations.map((task) => renderTask(task, true))
            ) : (
              <p className="px-2 py-2 text-ui-sm text-muted-foreground">
                {tr("No conversations")}
              </p>
            ),
            <div className="flex items-center gap-1">
              <Button
                size="icon-sm"
                variant="ghost"
                className="size-6"
                onClick={onNewIndependentConversation}
                disabled={locked || !ready}
                title={tr("New independent conversation")}
                aria-label={tr("New independent conversation")}
              >
                <HugeiconsIcon icon={Add01Icon} size={14} strokeWidth={1.75} />
              </Button>
            </div>,
          )}
        </div>
      </ScrollArea>
      {locked && (
        <p className="border-t border-border/60 px-3 py-2 text-ui-sm leading-relaxed text-muted-foreground">
          {tr(
            pendingEdits
              ? "Review pending changes before switching tasks."
              : "Stop the agent or resolve approvals before switching tasks.",
          )}
        </p>
      )}
      {reorder.dragging &&
        createPortal(
          <div
            ref={reorder.ghostRef}
            aria-hidden="true"
            className="pointer-events-none fixed left-0 top-0 z-50 flex max-w-64 items-center gap-2 rounded-sm border border-border/70 bg-card/95 px-2 py-1 text-ui-base text-foreground shadow-md"
          >
            <HugeiconsIcon
              icon={
                reorder.dragging.source.kind === "task"
                  ? BubbleChatIcon
                  : Folder01Icon
              }
              size={14}
              className="shrink-0 text-muted-foreground"
            />
            <span className="truncate">{reorder.dragging.label}</span>
          </div>,
          document.body,
        )}
    </div>
  );
}

function ProjectDropLine({ edge }: { edge?: ProjectDropEdge }) {
  return edge ? (
    <div
      aria-hidden="true"
      className={cn(
        "pointer-events-none absolute inset-x-2 z-10 h-px bg-ring",
        edge === "before" ? "top-0" : "bottom-0",
      )}
    />
  ) : null;
}

function SidebarSection({
  id,
  title,
  collapsed,
  disabled,
  onToggle,
  action,
  children,
}: {
  id: SidebarSectionId;
  title: string;
  collapsed: boolean;
  disabled: boolean;
  onToggle: () => void;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section data-sidebar-section={id}>
      <div className="group/section-header flex min-h-8 items-center gap-1 px-2">
        <button
          type="button"
          className="flex min-w-0 flex-1 items-center gap-1.5 rounded-sm py-1 text-left text-ui-base font-medium text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          aria-expanded={!collapsed}
          aria-controls={`sidebar-section-${id}`}
          disabled={disabled}
          onClick={onToggle}
        >
          {title}
          <HugeiconsIcon
            icon={collapsed ? ArrowRight01Icon : ArrowDown01Icon}
            size={12}
            strokeWidth={1.75}
            aria-hidden="true"
          />
        </button>
        {action}
      </div>
      <div
        id={`sidebar-section-${id}`}
        hidden={collapsed}
        className="space-y-0.5"
      >
        {!collapsed && children}
      </div>
    </section>
  );
}

function TaskRow({
  task,
  active,
  locked,
  status,
  onSelect,
  topLevel,
  pinned,
  pinDisabled,
  onTogglePin,
  dropEdge,
  dragging,
  onPointerDown,
  onMoveWithKeyboard,
}: {
  task: SessionMeta;
  active: boolean;
  locked: boolean;
  status: string;
  onSelect: () => void;
  topLevel: boolean;
  pinned: boolean;
  pinDisabled: boolean;
  onTogglePin: () => void;
  dropEdge?: ProjectDropEdge;
  dragging: boolean;
  onPointerDown?: (event: PointerEvent<HTMLButtonElement>) => void;
  onMoveWithKeyboard?: (direction: "up" | "down") => void;
}) {
  const tr = useTranslation();
  const [editing, setEditing] = useState(false);
  const label = task.title === "New chat" ? tr("New task") : task.title;
  return (
    <div
      data-sidebar-drop-id={task.id}
      data-sidebar-drop-kind="task"
      data-sidebar-drop-section={
        pinned ? "pinned" : task.projectless ? "conversations" : "projects"
      }
      data-sidebar-drop-owner={pinned ? undefined : conversationOwner(task)}
      className={cn(
        "group relative flex min-h-9 items-center gap-1 rounded-md pr-2 text-ui-base font-normal text-foreground",
        topLevel ? "pl-2" : "pl-8",
        dragging && "opacity-50",
        active
          ? "bg-foreground/[0.07] dark:bg-foreground/[0.09]"
          : "hover:bg-foreground/[0.045]",
      )}
    >
      <ProjectDropLine edge={dropEdge} />
      {topLevel && (
        <HugeiconsIcon
          icon={BubbleChatIcon}
          size={14}
          strokeWidth={1.75}
          className="mr-1 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
      )}
      {editing ? (
        <RenameInput
          label={tr("Rename task")}
          initial={label}
          onCommit={(value) => {
            if (value) useChatStore.getState().renameSession(task.id, value);
            setEditing(false);
          }}
        />
      ) : (
        <button
          type="button"
          aria-current={active ? "page" : undefined}
          disabled={task.archived || (locked && !active)}
          onClick={onSelect}
          onPointerDown={onPointerDown}
          onKeyDown={(event) => {
            if (
              onMoveWithKeyboard &&
              event.altKey &&
              (event.key === "ArrowUp" || event.key === "ArrowDown")
            ) {
              event.preventDefault();
              onMoveWithKeyboard(event.key === "ArrowUp" ? "up" : "down");
            }
          }}
          title={
            onPointerDown
              ? `${label}\n${tr("Drag to reorder conversations. Alt + Up/Down also moves the conversation.")}`
              : label
          }
          className={cn(
            "min-w-0 flex-1 truncate py-1.5 text-left disabled:opacity-50",
            active && "font-medium",
            onPointerDown && "touch-none select-none",
          )}
        >
          {label}
        </button>
      )}
      {(status === "thinking" || status === "streaming") && (
        <Spinner className="size-3 shrink-0" />
      )}
      {status === "awaiting-approval" && (
        <span className="text-ui-xs text-muted-foreground">
          {tr("needs approval")}
        </span>
      )}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon-sm"
            className="size-5 shrink-0 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 [@media(hover:none)]:opacity-100"
            aria-label={tr("Task actions")}
            disabled={locked}
          >
            <HugeiconsIcon icon={MoreHorizontalIcon} size={12} />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem disabled={pinDisabled} onSelect={onTogglePin}>
            {tr(pinned ? "Unpin" : "Pin conversation")}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setEditing(true)}>
            {tr("Rename task")}
          </DropdownMenuItem>
          <DropdownMenuItem
            onSelect={() =>
              useChatStore.getState().archiveSession(task.id, !task.archived)
            }
          >
            {tr(task.archived ? "Restore task" : "Archive task")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

// 项目重命名需同步覆盖 Input 的桌面字号，避免编辑时恢复为 14px。
function RenameInput({
  label,
  initial,
  className,
  onCommit,
}: {
  label: string;
  initial: string;
  className?: string;
  onCommit: (value: string) => void;
}) {
  const [value, setValue] = useState(initial);
  return (
    <Input
      autoFocus
      aria-label={label}
      value={value}
      onChange={(e) => setValue(e.target.value)}
      onFocus={(e) => e.target.select()}
      className={cn(
        "h-auto min-h-6 min-w-0 flex-1 px-1.5 py-0.5 text-ui-base",
        className,
      )}
      onBlur={() => onCommit(value.trim())}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") onCommit(value.trim());
        if (e.key === "Escape") onCommit("");
      }}
    />
  );
}

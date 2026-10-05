/* eslint-disable max-lines -- workspace 行同时承载折叠和快捷操作，先保持同文件收口。 */
import {
  memo,
  useCallback,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent,
} from "react";
import {
  CircleAlert,
  Ellipsis,
  Folder,
  FolderOpen,
  House,
  ListTree,
  MessageCirclePlus,
  XIcon,
} from "lucide-react";
import type { useSortable } from "@dnd-kit/sortable";
import { STATUS_DOT } from "@/components/workflow-graph/run-status-presentation.js";
import { Button, buttonVariants } from "@/components/ui/button.js";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible.js";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu.js";
import { ControlHintTooltip } from "@/ControlHintTooltip.js";
import { useZCodeIntl } from "@/i18n/IntlProvider.js";
import { TaskList } from "@/TaskList.js";
import { selectWorkspaceZCodeState, useZCodeSessionStore } from "@/store/zcodeSessionStore.js";
import type { WorkspaceTabState } from "@/store/tabStore.js";
import { cn } from "@/components/lib/utils.js";
import {
  TID_WORKSPACE_CLOSE,
  TID_WORKSPACE_FILE_TREE_BUTTON,
  TID_WORKSPACE_ITEM,
  testId,
} from "@zcode/shared";
import type { ZCodeTaskMeta } from "@zcode/shared";
import { useBaseWorkspaceServices } from "@/hooks/useWorkspaceServices.js";
import {
  applyTaskQueryCacheMutation,
  invalidateTaskQueryCacheByScopes,
} from "@/store/taskQueryCacheStore.js";
import { logger } from "@/logger.js";
import { TaskRowActionButton } from "@/workspace-grouped-tasks/task-row-action-button.js";
import { releaseWorkspaceRuntimeAfterProjectRemoval } from "@/lib/workspaceRuntimeRelease.js";
import {
  hasRunningWorkspaceChat,
  scanWindowsReservedDeviceNameFiles,
} from "@/lib/workspaceRemovalSafety.js";
import { useConfirmDialog } from "@/hooks/useConfirmDialog.js";
import { toast } from "@/components/ui/toast.js";

export type SortableBindings = Pick<ReturnType<typeof useSortable>, "attributes" | "listeners">;

// workspace 行在流式工具事件期间会因父级刷新而重渲染；
// TaskList 如果每次收到新的空数组，会把等价数据误判成变化并连带刷新任务行。
const EMPTY_PINNED_TASKS: ZCodeTaskMeta[] = [];

function isHomeWorkspacePath(path: string): boolean {
  const normalizedPath = path.replace(/\\/g, "/").replace(/\/+$/, "");
  return /^(\/Users\/[^/]+|\/home\/[^/]+|[A-Za-z]:\/Users\/[^/]+)$/.test(normalizedPath);
}

export const WorkspaceSidebarItem = memo(function WorkspaceSidebarItem({
  tab,
  isActiveWorkspace,
  isExpanded,
  closeTab,
  toggleWorkspaceExpanded,
  onSelectTask,
  onStartDraftInWorkspace,
  taskItems,
  taskListLoading,
  taskListHasMore,
  taskListHasUnread = false,
  taskListLiveWorkflowCount = 0,
  onShowMoreTasks,
  onOpenFileTree,
  itemRef,
  itemStyle,
  sortableBindings,
  isDragging = false,
}: {
  tab: WorkspaceTabState;
  isActiveWorkspace: boolean;
  isExpanded: boolean;
  activateTab: (tabId: string) => void;
  closeTab: (tabId: string) => void;
  toggleWorkspaceExpanded: (workspacePath: string) => void;
  onSelectTask: (
    targetWorkspacePath: string,
    taskId: string,
    targetWorkspaceIdentity?: string,
  ) => void;
  onStartDraftInWorkspace: (targetWorkspacePath: string, targetWorkspaceIdentity?: string) => void;
  taskItems: ZCodeTaskMeta[];
  taskListLoading: boolean;
  taskListHasMore: boolean;
  taskListHasUnread?: boolean;
  /** 组内在跑的工作流 run 数；项目收起时在未读点旁画脉冲灯（>1 带数量）。 */
  taskListLiveWorkflowCount?: number;
  onShowMoreTasks: () => void;
  onOpenFileTree?: (target: {
    workspacePath: string;
    workspaceName: string;
    workspaceIdentity?: string;
  }) => void;
  itemRef?: (node: HTMLLIElement | null) => void;
  itemStyle?: CSSProperties;
  sortableBindings?: SortableBindings;
  isDragging?: boolean;
}) {
  const { intl } = useZCodeIntl();
  const workspaceZCodeState = useZCodeSessionStore((state) =>
    selectWorkspaceZCodeState(state, tab.workspacePath, tab.workspaceIdentity),
  );
  const activeTaskId = workspaceZCodeState.activeTaskId;
  const removeTaskState = useZCodeSessionStore((state) => state.removeTaskState);
  const upsertOptimisticTaskListItem = useZCodeSessionStore(
    (state) => state.upsertOptimisticTaskListItem,
  );
  const removeOptimisticTaskListItem = useZCodeSessionStore(
    (state) => state.removeOptimisticTaskListItem,
  );
  const setTaskUnreadIndicator = useZCodeSessionStore((state) => state.setTaskUnreadIndicator);
  const baseServices = useBaseWorkspaceServices();
  const confirmDialog = useConfirmDialog();
  const zcodeTaskService = baseServices.zcodeTaskService;
  const taskItemsRef = useRef(taskItems);
  taskItemsRef.current = taskItems;
  const workspaceZCodeStateRef = useRef(workspaceZCodeState);
  workspaceZCodeStateRef.current = workspaceZCodeState;
  const findCurrentTaskItem = useCallback((taskId: string) => {
    // 流式刷新会重建 taskItems 数组，任务操作回调如果直接依赖数组，
    // 即使任务语义没变也会换引用，继续击穿 TaskListItem 的 memo。
    // 用 ref 在调用时读取最新列表，既保持回调稳定，也避免乐观更新拿到过期 meta。
    return taskItemsRef.current.find((task) => task.taskId === taskId) ?? null;
  }, []);
  const isHomeWorkspace = isHomeWorkspacePath(tab.workspacePath);
  const readOnlyReason =
    tab.availability === "unavailable-local-directory"
      ? intl.formatMessage({ id: "workspaceSidebar.unavailableLocalDirectory" })
      : undefined;
  const [workspaceRowHovered, setWorkspaceRowHovered] = useState(false);
  const [workspaceRowFocusWithin, setWorkspaceRowFocusWithin] = useState(false);
  const [workspaceActionMenuOpen, setWorkspaceActionMenuOpen] = useState(false);
  const [isHoverNone] = useState(
    () =>
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia("(hover: none)").matches,
  );
  const shouldMountWorkspaceRowActions =
    // workspace action 以前常驻 DOM，仅靠 opacity 隐藏；相邻 tooltip 会在
    // 浮层定位完成前误认隐藏 trigger，短暂显示到错误位置。改为交互时挂载，菜单打开时保活。
    workspaceRowHovered || workspaceRowFocusWithin || workspaceActionMenuOpen || isHoverNone;
  const handleWorkspaceOpenChange = useCallback(
    (nextOpen: boolean) => {
      // workspace 草稿导航本身会把 workspace 标记为展开。
      // 之前在 Collapsible 的 onOpenChange 里无论展开/收起都先激活 workspace，
      // 收起后的下一次点击会先被激活路径展开，再被 toggleWorkspaceExpanded 反向切回收起，
      // 表现出来就是"收起后再也打不开"。
      // 这里把职责拆开：展开时只走草稿导航；收起时只做 toggle，避免两条状态更新互相抵消。
      if (nextOpen) {
        if (!isExpanded) {
          // workspace 行表达“打开这个 workspace”，不是恢复它上次选中的 session。
          // 统一走上层草稿导航事务，让 workspace identity、group/pane 清理和 draft 聚焦一起收口。
          onStartDraftInWorkspace(tab.workspacePath, tab.workspaceIdentity);
        }
      } else if (isExpanded) {
        toggleWorkspaceExpanded(tab.workspacePath);
      }
    },
    [
      isExpanded,
      onStartDraftInWorkspace,
      tab.workspaceIdentity,
      tab.workspacePath,
      toggleWorkspaceExpanded,
    ],
  );

  const handleSelectTask = useCallback(
    (taskId: string) => {
      // 性能优化：上层 handleSelectTask 已经会按 workspacePath 激活 tab。
      // 这里重复 activate 会额外触发一轮 tab store 更新，把整列 workspace 行都带着重渲染一次。
      onSelectTask(tab.workspacePath, taskId, tab.workspaceIdentity);
    },
    [onSelectTask, tab.workspaceIdentity, tab.workspacePath],
  );

  const handleActionMouseDown = useCallback((event: MouseEvent<HTMLElement>) => {
    event.preventDefault();
    event.stopPropagation();
  }, []);

  const handleActionMenuClick = useCallback((event: MouseEvent<HTMLElement>) => {
    // DropdownMenuContent 虽然通过 Portal 渲染到行外，React 合成 click 仍会沿组件树
    // 冒泡到外层 CollapsibleTrigger。收起的 workspace 点“移除”时，会先 closeTab，再被展开回调
    // 当成“打开 workspace”重新 addTab，表现为删不掉。菜单层统一截断 click，保留菜单选择与键盘语义。
    event.stopPropagation();
  }, []);

  const handleCreateThreadClick = useCallback(
    (event: MouseEvent<HTMLButtonElement>) => {
      event.preventDefault();
      event.stopPropagation();
      if (readOnlyReason) {
        return;
      }
      onStartDraftInWorkspace(tab.workspacePath, tab.workspaceIdentity);
    },
    [onStartDraftInWorkspace, readOnlyReason, tab.workspaceIdentity, tab.workspacePath],
  );

  const handleRemoveWorkspace = useCallback(async () => {
    const workspaceKey = tab.workspaceIdentity?.trim() || tab.workspacePath;
    logger.debug("[WorkspaceSidebarItem] 移除 workspace", {
      isExpanded,
      workspaceKey,
    });

    if (
      hasRunningWorkspaceChat({
        workspaceState: workspaceZCodeStateRef.current,
        taskItems: taskItemsRef.current,
      })
    ) {
      const confirmed = await confirmDialog({
        title: intl.formatMessage({ id: "workspaceSidebar.removeRunningWorkspace.title" }),
        description: intl.formatMessage({
          id: "workspaceSidebar.removeRunningWorkspace.description",
        }),
        confirmLabel: intl.formatMessage({ id: "workspaceSidebar.removeRunningWorkspace.confirm" }),
        cancelLabel: intl.formatMessage({ id: "common.cancel" }),
        confirmVariant: "destructive",
      });
      if (!confirmed) {
        logger.debug("[WorkspaceSidebarItem] 用户取消移除运行中 workspace", { workspaceKey });
        return;
      }
    }

    closeTab(tab.id);
    releaseWorkspaceRuntimeAfterProjectRemoval({
      tab: {
        workspacePath: tab.workspacePath,
        workspaceIdentity: tab.workspaceIdentity,
      },
      zcodeTaskService,
    });
    // 移除 workspace 只移除入口；任务索引仍由本地缓存层统一管理。
    invalidateTaskQueryCacheByScopes([
      {
        workspacePath: tab.workspacePath,
        ...(tab.workspaceIdentity ? { workspaceIdentity: tab.workspaceIdentity } : {}),
      },
    ]);

    void scanWindowsReservedDeviceNameFiles(baseServices.fileService, tab.workspacePath)
      .then((result) => {
        if (result.findings.length === 0) {
          return;
        }
        const firstFinding = result.findings[0] ?? tab.workspacePath;
        toast(
          intl.formatMessage(
            { id: "workspaceSidebar.windowsReservedNameRisk" },
            { count: result.findings.length, path: firstFinding },
          ),
          { durationMs: 8_000, variant: "warning" },
        );
      })
      .catch((error: unknown) => {
        // Windows 保留名扫描只是移除后的兼容风险提示，失败不能影响 workspace 生命周期释放。
        logger.debug("[WorkspaceSidebarItem] Windows 保留名风险扫描失败", {
          workspaceKey,
          error,
        });
      });
  }, [
    baseServices.fileService,
    closeTab,
    confirmDialog,
    intl,
    isExpanded,
    tab.id,
    tab.workspaceIdentity,
    tab.workspacePath,
    zcodeTaskService,
  ]);

  const handleOpenWorkspaceFileTree = useCallback(
    (event: MouseEvent<HTMLButtonElement>) => {
      event.preventDefault();
      event.stopPropagation();
      if (readOnlyReason || !onOpenFileTree) {
        return;
      }

      onOpenFileTree({
        workspacePath: tab.workspacePath,
        workspaceName: tab.label,
        workspaceIdentity: tab.workspaceIdentity,
      });
    },
    [
      onOpenFileTree,
      readOnlyReason,
      tab.label,
      tab.workspaceIdentity,
      tab.workspacePath,
    ],
  );

  // 这些 TaskList 操作以前在 JSX 中每次 render 都创建新闭包。
  // 流式事件刷新 workspace 行时，即使任务数据没变，也会穿透 TaskList/TaskListItem 的 memo。
  const handleRenameTask = useCallback(
    async (taskId: string, title: string) => {
      if (readOnlyReason) {
        return null;
      }
      const previousTask = findCurrentTaskItem(taskId);
      logger.info("[WorkspaceSidebarItem] rename service call start", {
        taskId,
        workspacePath: tab.workspacePath,
        workspaceIdentity: tab.workspaceIdentity,
        previousTitleLength: previousTask?.title.length,
        nextTitleLength: title.length,
      });
      let meta: ZCodeTaskMeta;
      try {
        meta = await zcodeTaskService.renameTask({
          taskId,
          workspacePath: tab.workspacePath,
          title,
          ...(tab.workspaceIdentity ? { workspaceIdentity: tab.workspaceIdentity } : {}),
        });
      } catch (error) {
        logger.error("[WorkspaceSidebarItem] rename service call failed", {
          taskId,
          workspacePath: tab.workspacePath,
          workspaceIdentity: tab.workspaceIdentity,
          message: error instanceof Error ? error.message : String(error),
        });
        throw error;
      }
      logger.info("[WorkspaceSidebarItem] rename service call resolved", {
        taskId,
        workspacePath: tab.workspacePath,
        workspaceIdentity: tab.workspaceIdentity,
        resolvedTitleLength: meta.title.length,
      });
      upsertOptimisticTaskListItem(tab.workspacePath, meta, tab.workspaceIdentity);
      applyTaskQueryCacheMutation({
        previousTask: previousTask ?? meta,
        nextTask: meta,
        previousState: { pinned: false, archived: false },
        nextState: { pinned: false, archived: false },
      });
      logger.info("[WorkspaceSidebarItem] rename cache mutation applied", {
        taskId,
        workspacePath: tab.workspacePath,
        workspaceIdentity: tab.workspaceIdentity,
      });
      return meta;
    },
    [
      tab.workspaceIdentity,
      tab.workspacePath,
      findCurrentTaskItem,
      readOnlyReason,
      upsertOptimisticTaskListItem,
      zcodeTaskService,
    ],
  );

  const handleSetTaskPinned = useCallback(
    async (taskId: string, pinned: boolean) => {
      if (readOnlyReason) {
        return null;
      }
      const previousTask = findCurrentTaskItem(taskId);
      if (previousTask) {
        applyTaskQueryCacheMutation({
          previousTask,
          nextTask: previousTask,
          previousState: { pinned: false, archived: false },
          nextState: { pinned, archived: false },
        });
      }
      try {
        const meta = await zcodeTaskService.setTaskPinned({
          taskId,
          workspacePath: tab.workspacePath,
          pinned,
          ...(tab.workspaceIdentity ? { workspaceIdentity: tab.workspaceIdentity } : {}),
        });
        removeOptimisticTaskListItem(tab.workspacePath, taskId, tab.workspaceIdentity);
        applyTaskQueryCacheMutation({
          previousTask: previousTask ?? meta,
          nextTask: meta,
          previousState: { pinned, archived: false },
          nextState: { pinned, archived: false },
        });
        return meta;
      } catch (error) {
        if (previousTask) {
          applyTaskQueryCacheMutation({
            previousTask,
            nextTask: previousTask,
            previousState: { pinned, archived: false },
            nextState: { pinned: false, archived: false },
          });
        }
        throw error;
      }
    },
    [
      removeOptimisticTaskListItem,
      readOnlyReason,
      tab.workspaceIdentity,
      tab.workspacePath,
      findCurrentTaskItem,
      zcodeTaskService,
    ],
  );

  const handleArchiveTask = useCallback(
    async (taskId: string) => {
      if (readOnlyReason) {
        return null;
      }
      const previousTask = findCurrentTaskItem(taskId);
      const meta = await zcodeTaskService.archiveTask({
        taskId,
        workspacePath: tab.workspacePath,
        ...(tab.workspaceIdentity ? { workspaceIdentity: tab.workspaceIdentity } : {}),
      });
      removeTaskState(tab.workspacePath, taskId, tab.workspaceIdentity);
      applyTaskQueryCacheMutation({
        previousTask: previousTask ?? meta,
        nextTask: meta,
        previousState: { pinned: false, archived: false },
        nextState: { pinned: false, archived: true },
      });
      return meta;
    },
    [
      removeTaskState,
      readOnlyReason,
      tab.workspaceIdentity,
      tab.workspacePath,
      findCurrentTaskItem,
      zcodeTaskService,
    ],
  );

  const handleSetTaskUnread = useCallback(
    async (taskId: string, unread: boolean) => {
      if (readOnlyReason) {
        return null;
      }
      const previousTask = findCurrentTaskItem(taskId);
      const meta = await zcodeTaskService.setTaskUnread({
        taskId,
        workspacePath: tab.workspacePath,
        unread,
        ...(tab.workspaceIdentity ? { workspaceIdentity: tab.workspaceIdentity } : {}),
      });
      setTaskUnreadIndicator(tab.workspacePath, taskId, unread, tab.workspaceIdentity);
      upsertOptimisticTaskListItem(tab.workspacePath, meta, tab.workspaceIdentity);
      applyTaskQueryCacheMutation({
        previousTask: previousTask ?? meta,
        nextTask: meta,
        previousState: { pinned: false, archived: false },
        nextState: { pinned: false, archived: false },
      });
      return meta;
    },
    [
      setTaskUnreadIndicator,
      readOnlyReason,
      tab.workspaceIdentity,
      tab.workspacePath,
      findCurrentTaskItem,
      upsertOptimisticTaskListItem,
      zcodeTaskService,
    ],
  );

  const renderWorkspaceIcon = () => {
    // workspace 行之前在 hover/展开时会把目录图标切成箭头，
    // 视觉上会多出一层“树形展开控件”的暗示；当前交互只需要保留项目图标本身，
    // 这样能减少噪音，也避免用户把它理解成独立的箭头开关。
    if (isExpanded) {
      return isHomeWorkspace ? (
        <House className="h-4 w-4 text-foreground-subtle" />
      ) : (
        <FolderOpen className="h-4 w-4 text-foreground-subtle" />
      );
    }

    return isHomeWorkspace ? (
      <House className="h-4 w-4 text-foreground-subtle" />
    ) : (
      <Folder className="h-4 w-4 text-foreground-subtle" />
    );
  };

  const workspaceLabelContent = (
    <div className="flex min-w-0 flex-1 items-center gap-2">
      <span className="relative flex size-4 shrink-0 items-center justify-center">
        {renderWorkspaceIcon()}
      </span>
      <div className="min-w-0 truncate text-ui-base text-foreground-subtle">
        {tab.label}
      </div>
      {!isExpanded && taskListHasUnread ? (
        <span
          aria-hidden="true"
          data-workspace-unread-indicator="true"
          className="h-1.5 w-1.5 shrink-0 rounded-full bg-sky-500 dark:bg-sky-400"
        />
      ) : null}
      {!isExpanded && taskListLiveWorkflowCount > 0 ? (
        // 工作流运行行的组头汇总：
        // 只汇总在跑的 run；已结束未确认的行不上卷。
        <span
          data-workspace-workflow-indicator="true"
          data-count={String(taskListLiveWorkflowCount)}
          aria-label={intl.formatMessage(
            { id: "taskList.workflowRun.liveCount" },
            { count: String(taskListLiveWorkflowCount) },
          )}
          className="flex shrink-0 items-center gap-1 text-ui-xs leading-none text-foreground-subtle"
        >
          <span aria-hidden="true" className={cn("size-1.5 rounded-full", STATUS_DOT.running)} />
          {taskListLiveWorkflowCount > 1 ? taskListLiveWorkflowCount : null}
        </span>
      ) : null}
      {readOnlyReason ? (
        <ControlHintTooltip title={readOnlyReason} side="right" align="center">
          <span
            role="img"
            aria-label={readOnlyReason}
            tabIndex={0}
            className="flex size-4 shrink-0 items-center justify-center"
          >
            <CircleAlert className="size-3.5 text-destructive" />
          </span>
        </ControlHintTooltip>
      ) : null}
    </div>
  );

  return (
    <li ref={itemRef} style={itemStyle} className="space-y-2">
      <Collapsible
        className="flex flex-col gap-1"
        open={isExpanded}
        onOpenChange={handleWorkspaceOpenChange}
      >
        <div
          className={cn(
            "group flex items-center gap-2 rounded-lg transition-[background-color,box-shadow]",
            // "sticky top-0 z-10", // TODO: 拖拽时让 workspace 项悬浮 不要抹掉
            isDragging && "bg-selected shadow-xl",
          )}
        >
            <CollapsibleTrigger asChild>
              <div
                role="button"
                tabIndex={0}
                data-testid={testId(TID_WORKSPACE_ITEM, tab.workspacePath)}
                className={cn(
                  buttonVariants({ variant: "ghost", size: "default" }),
                  /*
                   * CollapsibleTrigger 会自动注入 aria-expanded。
                   * 这里复用了 ghost button 变体后，会命中全局 aria-expanded:bg-surface-hover
                   * 导致 workspace 项一展开就像"被选中"一样出现背景色。
                   * 局部把 aria-expanded 样式覆盖掉，只保留 hover，避免误导激活态。
                   */
                  "flex h-8 min-w-0 flex-1 justify-start gap-2 rounded-lg pl-2.5 pr-1 text-left text-foreground aria-expanded:bg-transparent aria-expanded:text-foreground",
                  "hover:bg-surface-hover hover:text-foreground",
                  sortableBindings && "cursor-grab active:cursor-grabbing",
                )}
                onMouseEnter={() => setWorkspaceRowHovered(true)}
                onMouseLeave={() => setWorkspaceRowHovered(false)}
                onFocusCapture={() => setWorkspaceRowFocusWithin(true)}
                onBlurCapture={(event) => {
                  if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
                    setWorkspaceRowFocusWithin(false);
                  }
                }}
                {...(sortableBindings?.attributes ?? {})}
                {...(sortableBindings?.listeners ?? {})}
              >
                {workspaceLabelContent}

                <div className="flex shrink-0 items-center gap-2">
                  <div className="flex shrink-0 items-center gap-1">
                    {shouldMountWorkspaceRowActions ? (
                      <DropdownMenu
                        open={workspaceActionMenuOpen}
                        onOpenChange={setWorkspaceActionMenuOpen}
                      >
                        <ControlHintTooltip title={intl.formatMessage({ id: "common.more" })}>
                          <DropdownMenuTrigger asChild>
                            <Button
                              type="button"
                              variant="ghost"
                              size="icon-sm"
                              className="shrink-0 text-foreground-subtle hover:bg-surface-hover hover:text-foreground"
                              onMouseDown={handleActionMouseDown}
                              aria-label={intl.formatMessage({ id: "common.more" })}
                            >
                              <Ellipsis className="h-3.5 w-3.5" />
                            </Button>
                          </DropdownMenuTrigger>
                        </ControlHintTooltip>
                        <DropdownMenuContent align="end" onClick={handleActionMenuClick}>
                          <DropdownMenuItem
                            data-testid={testId(TID_WORKSPACE_CLOSE, tab.workspacePath)}
                            onMouseDown={(event) => {
                              event.preventDefault();
                              event.stopPropagation();
                            }}
                            onSelect={(event) => {
                              event.preventDefault();
                              void handleRemoveWorkspace();
                            }}
                          >
                            <XIcon className="h-3.5 w-3.5" />
                            {intl.formatMessage({
                              id: "workspaceSidebar.remove",
                            })}
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    ) : null}
                    {shouldMountWorkspaceRowActions && onOpenFileTree ? (
                      <span className="shrink-0">
                        {/* Project 文件树入口以前单独覆盖 hover:bg-surface-hover，
                            与 Pinned / Grouped 的 bg-hover 不一致；三种入口统一复用同一 action。 */}
                        <TaskRowActionButton
                          // 该按钮默认继承 ghost 的主前景色，导致同组的三个图标明暗不一致。
                          className="text-foreground-subtle hover:text-foreground"
                          label={intl.formatMessage({
                            id: "workspaceSidebar.showFileTree",
                          })}
                          onClick={handleOpenWorkspaceFileTree}
                          showTooltip
                          disabledReason={readOnlyReason}
                          testId={testId(TID_WORKSPACE_FILE_TREE_BUTTON, tab.workspacePath)}
                        >
                          <ListTree className="h-3.5 w-3.5" />
                        </TaskRowActionButton>
                      </span>
                    ) : null}
                    {shouldMountWorkspaceRowActions ? (
                      <ControlHintTooltip
                        title={readOnlyReason ?? intl.formatMessage({ id: "taskList.newThread" })}
                      >
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon-sm"
                          className="shrink-0 text-foreground-subtle hover:bg-surface-hover hover:text-foreground"
                          onMouseDown={handleActionMouseDown}
                          onClick={handleCreateThreadClick}
                          disabled={Boolean(readOnlyReason)}
                          aria-label={intl.formatMessage({
                            id: "taskList.newThread",
                          })}
                        >
                          <MessageCirclePlus className="h-3.5 w-3.5" />
                        </Button>
                      </ControlHintTooltip>
                    ) : null}
                  </div>
                </div>
              </div>
            </CollapsibleTrigger>
          </div>

        <CollapsibleContent>
          <TaskList
            workspacePath={tab.workspacePath}
            workspaceIdentity={tab.workspaceIdentity}
            tasks={taskItems}
            pinnedTasks={EMPTY_PINNED_TASKS}
            activeTaskId={isActiveWorkspace ? activeTaskId : null}
            onSelectTask={handleSelectTask}
            showCreateButton={false}
            showFooter={false}
            loading={taskListLoading}
            hasMore={taskListHasMore}
            onShowMore={onShowMoreTasks}
            onRenameTask={handleRenameTask}
            onSetTaskPinned={handleSetTaskPinned}
            onArchiveTask={handleArchiveTask}
            onSetTaskUnread={handleSetTaskUnread}
            readOnlyReason={readOnlyReason}
          />
        </CollapsibleContent>
      </Collapsible>
    </li>
  );
});
WorkspaceSidebarItem.displayName = "WorkspaceSidebarItem";

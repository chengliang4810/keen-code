import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Button } from "@/components/ui/button";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import {
  IconAlertTriangle,
  IconCheck,
  IconChevronRight,
  IconClose,
  IconFileDiff,
  IconGitBranch,
  IconGitCommit,
  IconLoader,
  IconPlus,
  IconPush,
  IconStopFilled,
  IconTerminal,
} from "@/components/icons";
import { createT, type Locale } from "@/i18n";
import { listenAcp } from "@/lib/acp/api";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import * as api from "@/lib/api";
import { Checkbox } from "@/components/ui/checkbox";
import { SubagentRow } from "@/components/SubagentRow";
import { AgentAvatar } from "@/components/AgentAvatar";
import { GlassModal } from "@/components/GlassModal";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { SearchField } from "@/components/SearchField";

type GitAction = "commit" | "commit-push" | "push";

/** 会改变后台任务列表的 Peri unstable-event 事件。 */
const BACKGROUND_TASK_UNSTABLE_EVENTS: ReadonlySet<string> = new Set([
  "bg-task-started",
  "bg-task-completed",
  "bg-task-cancelled",
  "bg-task-interacted",
]);

/** 判断一个 ACP unstable-event 是否要求刷新后台任务快照。 */
export function isBackgroundTaskUnstableEvent(
  event: string | null | undefined,
): boolean {
  return typeof event === "string" && BACKGROUND_TASK_UNSTABLE_EVENTS.has(event);
}

/** 全部后台任务操作的成功或失败反馈。 */
type BackgroundTasksFeedback = {
  /** 反馈类型。 */
  kind: "success" | "error";
  /** 面向用户展示的反馈文案。 */
  message: string;
};

export function groupSummarySubagents(subagents: AcpSubagentInfo[]) {
  const byStartedAt = (left: AcpSubagentInfo, right: AcpSubagentInfo) =>
    left.started_at - right.started_at;
  return {
    running: subagents
      .filter((agent) => agent.status === "running")
      .sort(byStartedAt),
    failed: subagents
      .filter((agent) => agent.status === "failed")
      .sort(byStartedAt),
    done: subagents
      .filter((agent) => agent.status === "done")
      .sort(byStartedAt),
  };
}

/** 摘要面板只展示当前根 Session 中仍在运行的后台 Shell。 */
export function summaryShellTasks(
  tasks: api.BackgroundTaskInfo[],
  sessionId: string | null,
) {
  if (!sessionId) return [];
  return tasks.filter(
    (task) => task.sessionId === sessionId && task.kind === "shell",
  );
}

/** 仅按 childThreadId 精确映射当前会话中运行的 Agent 任务。 */
export function summaryAgentTaskMap(
  tasks: api.BackgroundTaskInfo[],
  sessionId: string | null,
) {
  const byThreadId = new Map<string, api.BackgroundTaskInfo>();
  if (!sessionId) return byThreadId;
  for (const task of tasks) {
    if (
      task.sessionId === sessionId &&
      task.kind === "agent" &&
      task.childThreadId
    ) {
      byThreadId.set(task.childThreadId, task);
    }
  }
  return byThreadId;
}

function errorMessage(value: unknown): string {
  if (value instanceof Error) return value.message;
  return typeof value === "string" ? value : String(value);
}

/** 判断一次文档点击是否发生在任务摘要面板及其 portal 弹层以外。 */
export function shouldCloseConversationSummaryPanel(
  panel: Pick<HTMLElement, "contains"> | null,
  trigger: Pick<HTMLElement, "contains"> | null,
  target: EventTarget | null,
): boolean {
  if (!panel || !target) return false;
  const targetNode = target as Node;
  if (
    (targetNode as Element).closest?.(
      ".summary-panel__branch-surface, .summary-panel__background-tasks-surface",
    )
  ) {
    return false;
  }
  return !panel.contains(targetNode) && !trigger?.contains(targetNode);
}

export interface ConversationSummaryPanelProps {
  /** 是否显示任务摘要面板。 */
  open: boolean;
  /** 是否允许点击面板外部关闭摘要。 */
  dismissOnOutsidePress?: boolean;
  /** 打开任务摘要面板的按钮引用，用于排除按钮自身的指针事件。 */
  triggerRef: { readonly current: HTMLElement | null };
  /** 当前根 Session 所属项目目录。 */
  projectPath: string | null;
  /** 当前根 Session 标识。 */
  sessionId: string | null;
  /** 当前根 Session 的运行状态。 */
  sessionState: string;
  /** 当前根 Session 已投影的子 Agent。 */
  subagents: AcpSubagentInfo[];
  /** 当前界面语言。 */
  locale: Locale;
  /** 关闭任务摘要面板。 */
  onClose: () => void;
  /** 打开当前项目的变更视图。 */
  onOpenChanges: () => void;
  /** 在右侧资源栏打开子 Agent。 */
  onOpenSubagent: (agentId: string) => void;
  /** 在右侧资源栏打开完整子 Agent 列表。 */
  onOpenSubagentList: () => void;
}

export function ConversationSummaryPanel({
  open,
  dismissOnOutsidePress = false,
  triggerRef,
  projectPath,
  sessionId,
  sessionState,
  subagents,
  locale,
  onClose,
  onOpenChanges,
  onOpenSubagent,
  onOpenSubagentList,
}: ConversationSummaryPanelProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [git, setGit] = useState<api.GitStatusResult | null>(null);
  const [gitFormOpen, setGitFormOpen] = useState(false);
  const [branchMenuOpen, setBranchMenuOpen] = useState(false);
  const [branchSearch, setBranchSearch] = useState("");
  const [branchBusy, setBranchBusy] = useState<string | null>(null);
  const [branchError, setBranchError] = useState<string | null>(null);
  const [createBranchOpen, setCreateBranchOpen] = useState(false);
  const [newBranchName, setNewBranchName] = useState("work/");
  const [commitMessage, setCommitMessage] = useState("");
  const [includeUnstaged, setIncludeUnstaged] = useState(true);
  const [gitAction, setGitAction] = useState<GitAction | null>(null);
  const [gitFeedback, setGitFeedback] = useState<{
    kind: "success" | "error";
    message: string;
  } | null>(null);
  const [shellTasks, setShellTasks] = useState<api.BackgroundTaskInfo[]>([]);
  /** 最近一次 backgroundTasksList 返回的全部后台任务快照。 */
  const [backgroundTasks, setBackgroundTasks] = useState<
    api.BackgroundTaskInfo[]
  >([]);
  /** 全部后台任务取消操作是否正在执行。 */
  const [cancelAllBusy, setCancelAllBusy] = useState(false);
  /** 是否显示全部后台任务取消确认弹窗。 */
  const [cancelAllOpen, setCancelAllOpen] = useState(false);
  /** 全部后台任务取消操作的成功或失败反馈。 */
  const [cancelAllFeedback, setCancelAllFeedback] =
    useState<BackgroundTasksFeedback | null>(null);
  const [stoppingShellTaskIds, setStoppingShellTaskIds] = useState<Set<string>>(
    () => new Set(),
  );
  const [shellTaskError, setShellTaskError] = useState<string | null>(null);
  const [stoppingAgentTaskIds, setStoppingAgentTaskIds] = useState<Set<string>>(
    () => new Set(),
  );
  const [agentTaskError, setAgentTaskError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const gitRequest = useRef(0);
  const shellTaskRequest = useRef(0);
  const gitActionRef = useRef<GitAction | null>(null);
  const previousSessionState = useRef(sessionState);
  const panelRef = useRef<HTMLElement>(null);
  /** 当前摘要视图身份；异步后台任务结果只能回写到发起它的视图。 */
  const backgroundTaskViewKey = JSON.stringify([
    projectPath,
    sessionId,
    open,
  ]);
  /** 同步记录最新摘要视图，避免切换会话后旧闭包继续写入状态。 */
  const backgroundTaskViewRef = useRef(backgroundTaskViewKey);
  backgroundTaskViewRef.current = backgroundTaskViewKey;
  /** 后台任务变更操作的视图代次，关闭或切换会话时使旧操作失效。 */
  const backgroundTaskMutationEpochRef = useRef(0);
  /** 全部取消操作的同步忙碌锁，避免同一事件循环内重复提交。 */
  const cancelAllBusyRef = useRef(false);
  /** 当前持有全部取消忙碌锁的操作编号，用于迟到收尾时安全释放锁。 */
  const cancelAllBusyOperationRef = useRef<number | null>(null);
  /** 全部取消操作编号，用于隔离已失效的异步结果。 */
  const cancelAllOperationRef = useRef(0);
  /** 逐项取消操作的任务锁，值为唯一操作号，避免旧操作释放新操作的锁。 */
  const stoppingTaskOperationRef = useRef(new Map<string, number>());
  /** 逐项取消操作的唯一编号生成器。 */
  const stoppingTaskOperationSequenceRef = useRef(0);
  /** 仍在运行的逐项取消操作数量；跨 Session 保留以阻塞全局并发取消。 */
  const [individualStopBusyCount, setIndividualStopBusyCount] = useState(0);

  /** 刷新摘要 Git 状态；终态刷新可跳过短时缓存。 */
  const refreshGit = useCallback(async (force = false) => {
    const request = ++gitRequest.current;
    if (!projectPath) {
      setGit(null);
      return;
    }
    if (!api.isTauri()) {
      setGit(null);
      return;
    }
    try {
      const result = await api.gitStatus(projectPath, { force });
      if (request !== gitRequest.current) return;
      setGit(result.available ? result : null);
    } catch {
      if (request !== gitRequest.current) return;
      setGit(null);
    }
  }, [projectPath]);

  /** 刷新完整后台任务快照，并派生当前会话的 Shell 列表。 */
  const refreshShellTasks = useCallback(async (preserveError = false) => {
    const request = ++shellTaskRequest.current;
    const requestViewKey = backgroundTaskViewKey;
    const isCurrentRequest = () =>
      request === shellTaskRequest.current &&
      requestViewKey === backgroundTaskViewRef.current;
    if (!api.isTauri()) {
      if (!isCurrentRequest()) return;
      setShellTasks([]);
      setBackgroundTasks([]);
      if (!preserveError) {
        setShellTaskError(null);
        setAgentTaskError(null);
      }
      return;
    }
    try {
      const allTasks = await api.backgroundTasksList();
      if (!isCurrentRequest()) return;
      setShellTasks(summaryShellTasks(allTasks, sessionId));
      setBackgroundTasks(allTasks);
      if (!preserveError) {
        setShellTaskError(null);
        setAgentTaskError(null);
      }
    } catch (error) {
      if (!isCurrentRequest()) return;
      const message = errorMessage(error);
      setShellTaskError((current) => (preserveError && current ? current : message));
      setAgentTaskError((current) => (preserveError && current ? current : message));
    }
  }, [backgroundTaskViewKey, sessionId]);

  useEffect(() => {
    setGitFormOpen(false);
    setBranchMenuOpen(false);
    setCreateBranchOpen(false);
    setBranchError(null);
    setGitFeedback(null);
    setShellTasks([]);
    setBackgroundTasks([]);
    setCancelAllOpen(false);
    setCancelAllFeedback(null);
    setStoppingShellTaskIds(new Set());
    setShellTaskError(null);
    setStoppingAgentTaskIds(new Set());
    setAgentTaskError(null);
    shellTaskRequest.current += 1;
  }, [projectPath, sessionId]);

  useEffect(() => {
    backgroundTaskMutationEpochRef.current += 1;
    cancelAllOperationRef.current += 1;
  }, [open, projectPath, sessionId]);

  useEffect(() => {
    if (!open) {
      gitRequest.current += 1;
      shellTaskRequest.current += 1;
      setCancelAllOpen(false);
      setCancelAllFeedback(null);
      setStoppingShellTaskIds(new Set());
      setStoppingAgentTaskIds(new Set());
      setShellTaskError(null);
      setAgentTaskError(null);
      return;
    }
    setGitFeedback(null);
    setCancelAllFeedback(null);
    void refreshGit();
    void refreshShellTasks();
  }, [open, refreshGit, refreshShellTasks]);

  useEffect(() => {
    if (!open) return;
    const timer = window.setInterval(
      () => void refreshShellTasks(true),
      1_000,
    );
    return () => window.clearInterval(timer);
  }, [open, refreshShellTasks]);

  useEffect(() => {
    if (!open || !api.isTauri()) return;
    let disposed = false;
    let unlisten: (() => void) | null = null;
    let refreshInFlight = false;
    let refreshQueued = false;
    const listenerViewKey = backgroundTaskViewKey;
    const isCurrentListener = () =>
      !disposed && listenerViewKey === backgroundTaskViewRef.current;
    const refreshFromEvent = () => {
      if (!isCurrentListener()) return;
      if (refreshInFlight) {
        refreshQueued = true;
        return;
      }
      refreshInFlight = true;
      void refreshShellTasks(true)
        .catch(() => undefined)
        .finally(() => {
          refreshInFlight = false;
          if (!refreshQueued) return;
          refreshQueued = false;
          refreshFromEvent();
        });
    };

    void listenAcp("acp://unstable-event", (notification) => {
      const params = notification.params;
      if (
        !isCurrentListener() ||
        !params?.sessionId ||
        !isBackgroundTaskUnstableEvent(params.event)
      ) {
        return;
      }
      refreshFromEvent();
    })
      .then((dispose) => {
        if (!isCurrentListener()) {
          dispose();
          return;
        }
        unlisten = dispose;
      })
      .catch(() => {
        // 轮询仍是事件通道不可用时的可靠兜底，不能覆盖已有摘要错误。
      });

    return () => {
      disposed = true;
      refreshQueued = false;
      unlisten?.();
    };
  }, [backgroundTaskViewKey, open, refreshShellTasks]);

  useEffect(() => {
    const previous = previousSessionState.current;
    previousSessionState.current = sessionState;
    if (open && previous === "streaming" && sessionState !== "streaming") {
      void refreshGit(true);
    }
  }, [open, refreshGit, sessionState]);

  useEffect(() => {
    if (!open) return;
    const onDocumentPointerDown = (event: PointerEvent) => {
      if (
        shouldCloseConversationSummaryPanel(
          panelRef.current,
          triggerRef.current,
          event.target,
        )
      ) {
        onClose();
      }
    };
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape" && !event.defaultPrevented) onClose();
    };
    if (dismissOnOutsidePress) {
      document.addEventListener("pointerdown", onDocumentPointerDown, true);
    }
    window.addEventListener("keydown", onKeyDown);
    return () => {
      if (dismissOnOutsidePress) {
        document.removeEventListener("pointerdown", onDocumentPointerDown, true);
      }
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [dismissOnOutsidePress, onClose, open, triggerRef]);

  const hasRunningAgent = subagents.some((agent) => agent.status === "running");
  useEffect(() => {
    if (!open || !hasRunningAgent) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [hasRunningAgent, open]);

  const groupedSubagents = useMemo(
    () => groupSummarySubagents(subagents),
    [subagents],
  );
  const activeSubagents = [
    ...groupedSubagents.running,
    ...groupedSubagents.failed,
  ];
  const agentTaskByThreadId = useMemo(
    () => summaryAgentTaskMap(backgroundTasks, sessionId),
    [backgroundTasks, sessionId],
  );
  /** 后台任务首次查询失败时，即使没有可展示条目也必须保留错误反馈。 */
  const backgroundTaskError = shellTaskError ?? agentTaskError;

  const runGitAction = async (action: GitAction) => {
    if (!projectPath || gitActionRef.current) return;
    if (action !== "push" && !commitMessage.trim()) {
      setGitFeedback({
        kind: "error",
        message: tr("summary.git.messageRequired"),
      });
      return;
    }
    gitActionRef.current = action;
    setGitAction(action);
    setGitFeedback(null);
    let committed: api.GitCommitResult | null = null;
    try {
      if (action !== "push") {
        committed = await api.gitCommit({
          projectPath,
          message: commitMessage,
          includeUnstaged,
        });
        setCommitMessage("");
      }
      if (action !== "commit") {
        await api.gitPush(projectPath);
      }
      if (action === "commit") {
        setGitFeedback({
          kind: "success",
          message: tr("summary.git.committed", {
            commit: committed?.commit ?? "",
          }),
        });
      } else if (action === "commit-push") {
        setGitFeedback({
          kind: "success",
          message: tr("summary.git.committedAndPushed", {
            commit: committed?.commit ?? "",
          }),
        });
      } else {
        setGitFeedback({
          kind: "success",
          message: tr("summary.git.pushed"),
        });
      }
    } catch (error) {
      const detail = errorMessage(error);
      setGitFeedback({
        kind: "error",
        message: committed
          ? tr("summary.git.commitThenPushFailed", {
              commit: committed.commit,
              error: detail,
            })
          : detail,
      });
    } finally {
      gitActionRef.current = null;
      setGitAction(null);
      await refreshGit();
    }
  };

  const handleCommitKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void runGitAction("commit");
    }
  };

  const filteredBranches = (git?.branches ?? [])
    .filter((branch) =>
      branch.toLocaleLowerCase().includes(branchSearch.trim().toLocaleLowerCase()),
    )
    .sort((left, right) =>
      left === git?.branch ? -1 : right === git?.branch ? 1 : left.localeCompare(right),
    );
  const newBranchInvalid =
    !newBranchName.trim() || newBranchName.trim().endsWith("/");

  const checkoutBranch = async (branch: string, create = false) => {
    if (!projectPath || branchBusy) return;
    setBranchBusy(branch);
    setBranchError(null);
    setGitFeedback(null);
    try {
      await api.gitCheckoutBranch(projectPath, branch, create);
      setBranchMenuOpen(false);
      setCreateBranchOpen(false);
      setBranchSearch("");
      if (create) setNewBranchName("work/");
      await refreshGit(true);
    } catch (error) {
      const message = errorMessage(error);
      if (create) setBranchError(message);
      else setGitFeedback({ kind: "error", message });
    } finally {
      setBranchBusy(null);
    }
  };

  const stopShellTask = async (task: api.BackgroundTaskInfo) => {
    const taskKey = `${task.sessionId}\u0000${task.taskId}`;
    if (
      cancelAllBusyRef.current ||
      cancelAllBusy ||
      stoppingTaskOperationRef.current.has(taskKey)
    ) {
      return;
    }
    const operation = ++stoppingTaskOperationSequenceRef.current;
    const operationViewKey = backgroundTaskViewKey;
    const operationEpoch = backgroundTaskMutationEpochRef.current;
    const isCurrentOperation = () =>
      operationViewKey === backgroundTaskViewRef.current &&
      operationEpoch === backgroundTaskMutationEpochRef.current;
    stoppingTaskOperationRef.current.set(taskKey, operation);
    setIndividualStopBusyCount((current) => current + 1);
    setStoppingShellTaskIds((current) => new Set(current).add(task.taskId));
    setShellTaskError(null);
    try {
      await api.backgroundTaskCancel(task.sessionId, task.taskId);
      if (!isCurrentOperation()) return;
      await refreshShellTasks();
    } catch (error) {
      if (!isCurrentOperation()) return;
      setShellTaskError(errorMessage(error));
      await refreshShellTasks(true);
    } finally {
      if (stoppingTaskOperationRef.current.get(taskKey) === operation) {
        stoppingTaskOperationRef.current.delete(taskKey);
        setIndividualStopBusyCount((current) => Math.max(0, current - 1));
      }
      if (isCurrentOperation()) {
        setStoppingShellTaskIds((current) => {
          const next = new Set(current);
          next.delete(task.taskId);
          return next;
        });
      }
    }
  };

  const stopAgentTask = async (task: api.BackgroundTaskInfo) => {
    const taskKey = `${task.sessionId}\u0000${task.taskId}`;
    if (
      cancelAllBusyRef.current ||
      cancelAllBusy ||
      stoppingTaskOperationRef.current.has(taskKey)
    ) {
      return;
    }
    const operation = ++stoppingTaskOperationSequenceRef.current;
    const operationViewKey = backgroundTaskViewKey;
    const operationEpoch = backgroundTaskMutationEpochRef.current;
    const isCurrentOperation = () =>
      operationViewKey === backgroundTaskViewRef.current &&
      operationEpoch === backgroundTaskMutationEpochRef.current;
    stoppingTaskOperationRef.current.set(taskKey, operation);
    setIndividualStopBusyCount((current) => current + 1);
    setStoppingAgentTaskIds((current) => new Set(current).add(task.taskId));
    setAgentTaskError(null);
    try {
      await api.backgroundTaskCancel(task.sessionId, task.taskId);
      if (!isCurrentOperation()) return;
      await refreshShellTasks();
    } catch (error) {
      if (!isCurrentOperation()) return;
      setAgentTaskError(
        tr("summary.subagents.stopFailed", { error: errorMessage(error) }),
      );
      await refreshShellTasks(true);
    } finally {
      if (stoppingTaskOperationRef.current.get(taskKey) === operation) {
        stoppingTaskOperationRef.current.delete(taskKey);
        setIndividualStopBusyCount((current) => Math.max(0, current - 1));
      }
      if (isCurrentOperation()) {
        setStoppingAgentTaskIds((current) => {
          const next = new Set(current);
          next.delete(task.taskId);
          return next;
        });
      }
    }
  };

  /** 确认后取消全部会话的后台任务，并在成功或失败后刷新完整快照。 */
  const stopAllBackgroundTasks = async () => {
    if (
      cancelAllBusyRef.current ||
      cancelAllBusy ||
      stoppingShellTaskIds.size > 0 ||
      stoppingAgentTaskIds.size > 0 ||
      individualStopBusyCount > 0 ||
      backgroundTasks.length === 0
    ) {
      setCancelAllOpen(false);
      return;
    }
    const taskCount = backgroundTasks.length;
    const operation = ++cancelAllOperationRef.current;
    const operationViewKey = backgroundTaskViewKey;
    const operationEpoch = backgroundTaskMutationEpochRef.current;
    const isCurrentOperation = () =>
      operation === cancelAllOperationRef.current &&
      operationViewKey === backgroundTaskViewRef.current &&
      operationEpoch === backgroundTaskMutationEpochRef.current;
    cancelAllBusyRef.current = true;
    cancelAllBusyOperationRef.current = operation;
    setCancelAllBusy(true);
    setCancelAllFeedback(null);
    try {
      await api.backgroundTasksCancelAll();
      if (isCurrentOperation()) {
        setCancelAllFeedback({
          kind: "success",
          message: tr("summary.backgroundTasks.stopAllSuccess", {
            count: String(taskCount),
          }),
        });
      }
    } catch (error) {
      if (isCurrentOperation()) {
        setCancelAllFeedback({
          kind: "error",
          message: tr("summary.backgroundTasks.stopAllFailed", {
            error: errorMessage(error),
          }),
        });
      }
    } finally {
      if (isCurrentOperation()) {
        await refreshShellTasks(true);
      }
      if (cancelAllBusyOperationRef.current === operation) {
        cancelAllBusyOperationRef.current = null;
        cancelAllBusyRef.current = false;
        setCancelAllBusy(false);
        if (isCurrentOperation()) setCancelAllOpen(false);
      }
    }
  };

  if (!open) return null;

  return (
    <aside
      ref={panelRef}
      className="summary-panel"
      aria-label={tr("summary.title")}
    >
      <header className="summary-panel__header">
        <strong className="summary-panel__title">{tr("summary.title")}</strong>
        <Button
          type="button"
          className="summary-panel__icon-btn"
          aria-label={tr("common.close")}
          onClick={onClose}
        >
          <IconClose size={17} />
        </Button>
      </header>

      <div className="summary-panel__body">
        <div className="summary-panel__overview">
            {git ? (
              <div className="summary-panel__git">
                <Button
              type="button"
              className="summary-panel__row"
              disabled={!git?.available}
              onClick={onOpenChanges}
            >
              <span className="summary-panel__row-icon">
                <IconFileDiff size={18} />
              </span>
              <span className="summary-panel__row-label">
                {tr("summary.changes")}
              </span>
              <span className="summary-panel__diff-stat">
                <span className="is-addition">+{git?.additions ?? 0}</span>
                <span className="is-deletion">−{git?.deletions ?? 0}</span>
              </span>
                </Button>

                <DropdownMenu open={branchMenuOpen} onOpenChange={setBranchMenuOpen}>
                  <DropdownMenuTrigger asChild>
                    <Button
                      type="button"
                      className="summary-panel__row"
                      disabled={!git?.available || Boolean(branchBusy)}
                      aria-label={tr("summary.branches.open")}
                    >
              <span className="summary-panel__row-icon">
                <IconGitBranch size={18} />
              </span>
              <span className="summary-panel__row-label">
                {tr("summary.branch")}
              </span>
              <code className="summary-panel__branch">
                {git?.available
                  ? git.branch || tr("summary.branchUnavailable")
                  : "—"}
              </code>
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent
                    side="left"
                    align="start"
                    sideOffset={10}
                    className="summary-panel__branch-menu summary-panel__branch-surface"
                  >
                    <SearchField
                      containerClassName="summary-panel__branch-search"
                      iconSize={17}
                      value={branchSearch}
                      autoFocus
                      placeholder={tr("summary.branches.search")}
                      aria-label={tr("summary.branches.search")}
                      onChange={(event) => setBranchSearch(event.target.value)}
                      onKeyDown={(event) => event.stopPropagation()}
                    />
                    <div className="summary-panel__branch-heading">
                      {tr("summary.branches.title")}
                    </div>
                    <div className="summary-panel__branch-list">
                      {filteredBranches.map((branch) => (
                        <DropdownMenuItem
                          key={branch}
                          disabled={Boolean(branchBusy)}
                          onSelect={(event) => {
                            if (branch === git.branch) return event.preventDefault();
                            void checkoutBranch(branch);
                          }}
                        >
                          <IconGitBranch size={17} />
                          <span className="summary-panel__branch-item-label">
                            <span>{branch}</span>
                            {branch === git.branch && git.files.length ? (
                              <small>
                                {tr("summary.branches.uncommitted", {
                                  count: String(git.files.length),
                                })}
                              </small>
                            ) : null}
                          </span>
                          {branch === git.branch ? <IconCheck size={17} /> : null}
                        </DropdownMenuItem>
                      ))}
                    </div>
                    <DropdownMenuSeparator />
                    <DropdownMenuItem
                      onSelect={() => {
                        setBranchMenuOpen(false);
                        setCreateBranchOpen(true);
                      }}
                    >
                      <IconPlus size={18} />
                      {tr("summary.branches.create")}
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>

                <Button
              type="button"
              className={
                "summary-panel__row" + (gitFormOpen ? " is-active" : "")
              }
              aria-expanded={gitFormOpen}
              disabled={!git?.available}
              onClick={() => setGitFormOpen((value) => !value)}
            >
              <span className="summary-panel__row-icon">
                <IconGitCommit size={18} />
              </span>
              <span className="summary-panel__row-label">
                {tr("summary.commitOrPush")}
              </span>
              <IconChevronRight
                size={16}
                className={
                  gitFormOpen
                    ? "summary-panel__chevron is-open"
                    : "summary-panel__chevron"
                }
              />
                </Button>

                {gitFormOpen ? (
              <section className="summary-panel__git-form">
                <Label className="summary-panel__commit-field">
                  <span className="sr-only">
                    {tr("summary.git.message")}
                  </span>
                  <Input
                    value={commitMessage}
                    maxLength={4_096}
                    placeholder={tr("summary.git.messagePlaceholder")}
                    onChange={(event) => setCommitMessage(event.target.value)}
                    onKeyDown={handleCommitKeyDown}
                  />
                </Label>
                <div className="summary-panel__checkbox">
                  <Checkbox
                    id="summary-panel-include-unstaged"
                    checked={includeUnstaged}
                    disabled={!git?.hasUnstagedChanges || Boolean(gitAction)}
                    aria-label={tr("summary.git.includeUnstaged")}
                    onCheckedChange={(checked) =>
                      setIncludeUnstaged(checked === true)
                    }
                  />
                  <Label htmlFor="summary-panel-include-unstaged">
                    {tr("summary.git.includeUnstaged")}
                  </Label>
                  <span className="summary-panel__diff-stat">
                    <span className="is-addition">+{git?.additions ?? 0}</span>
                    <span className="is-deletion">−{git?.deletions ?? 0}</span>
                  </span>
                </div>
                <div className="summary-panel__git-actions">
                  <Button
                    type="button"
                    disabled={Boolean(gitAction) || !git?.files.length}
                    onClick={() => void runGitAction("commit")}
                  >
                    {gitAction === "commit" ? (
                      <IconLoader size={15} className="summary-panel__spin" />
                    ) : (
                      <IconGitCommit size={15} />
                    )}
                    {tr("summary.git.commit")}
                  </Button>
                  <Button
                    type="button"
                    disabled={Boolean(gitAction) || !git?.files.length}
                    onClick={() => void runGitAction("commit-push")}
                  >
                    {gitAction === "commit-push" ? (
                      <IconLoader size={15} className="summary-panel__spin" />
                    ) : (
                      <IconPush size={15} />
                    )}
                    {tr("summary.git.commitAndPush")}
                  </Button>
                  <Button
                    type="button"
                    disabled={Boolean(gitAction) || !git?.available}
                    onClick={() => void runGitAction("push")}
                  >
                    {gitAction === "push" ? (
                      <IconLoader size={15} className="summary-panel__spin" />
                    ) : (
                      <IconPush size={15} />
                    )}
                    {tr("summary.git.push")}
                  </Button>
                </div>
              </section>
                ) : null}

                {gitFeedback ? (
              <div
                className={
                  "summary-panel__notice" +
                  (gitFeedback.kind === "error" ? " is-error" : "")
                }
                role="status"
              >
                {gitFeedback.kind === "error" ? (
                  <IconAlertTriangle size={14} />
                ) : (
                  <IconCheck size={14} />
                )}
                <span>{gitFeedback.message}</span>
              </div>
                ) : null}
              </div>
            ) : null}

            {backgroundTasks.length > 0 ? (
              <section
                className="summary-panel__background-tasks"
                aria-labelledby="summary-panel-background-tasks-title"
              >
                {git ? <div className="summary-panel__divider" /> : null}
                <div className="summary-panel__background-toolbar">
                  <div
                    className="summary-panel__section-title summary-panel__background-heading"
                    id="summary-panel-background-tasks-title"
                  >
                    <span>{tr("summary.backgroundTasks.title")}</span>
                    <span className="summary-panel__diff-stat summary-panel__background-count">
                      {tr("summary.backgroundTasks.allSessionsCount", {
                        count: String(backgroundTasks.length),
                      })}
                    </span>
                  </div>
                  <Button
                    type="button"
                    className="btn btn--danger"
                    aria-label={tr("summary.backgroundTasks.stopAll")}
                    aria-busy={cancelAllBusy}
                    disabled={
                      cancelAllBusy ||
                      stoppingShellTaskIds.size > 0 ||
                      stoppingAgentTaskIds.size > 0 ||
                      individualStopBusyCount > 0
                    }
                    onClick={() => setCancelAllOpen(true)}
                  >
                    {cancelAllBusy ? (
                      <IconLoader size={14} className="summary-panel__spin" />
                    ) : (
                      <IconStopFilled size={14} />
                    )}
                    <span>{tr("summary.backgroundTasks.stopAll")}</span>
                  </Button>
                </div>
              </section>
            ) : null}

            {cancelAllFeedback ? (
              <div
                className={
                  "summary-panel__notice" +
                  (cancelAllFeedback.kind === "error" ? " is-error" : "")
                }
                role={cancelAllFeedback.kind === "error" ? "alert" : "status"}
              >
                {cancelAllFeedback.kind === "error" ? (
                  <IconAlertTriangle size={14} />
                ) : (
                  <IconCheck size={14} />
                )}
                <span>{cancelAllFeedback.message}</span>
              </div>
            ) : null}

            {!shellTasks.length && !subagents.length && backgroundTaskError ? (
              <div className="summary-panel__notice is-error" role="alert">
                <IconAlertTriangle size={14} />
                <span>{backgroundTaskError}</span>
              </div>
            ) : null}

            {shellTasks.length > 0 ? (
              <section
                className="summary-panel__shells"
                aria-labelledby="summary-panel-shells-title"
              >
                {git || backgroundTasks.length > 0 ? (
                  <div className="summary-panel__divider" />
                ) : null}
                <div
                  className="summary-panel__shell-heading"
                  id="summary-panel-shells-title"
                >
                  {tr("summary.backgroundShells.title")}
                </div>
                <div className="summary-panel__shell-list">
                  {shellTasks.map((task) => {
                    const stopping = stoppingShellTaskIds.has(task.taskId);
                    return (
                      <div
                        key={`${task.sessionId}:${task.taskId}`}
                        className="summary-panel__shell-row"
                      >
                        <span className="summary-panel__shell-icon">
                          <IconTerminal size={18} />
                        </span>
                        <span
                          className="summary-panel__shell-command"
                          title={task.summary}
                        >
                          {task.summary}
                        </span>
                        <Button
                          type="button"
                          className="summary-panel__shell-stop"
                          aria-label={tr("summary.backgroundShells.stop")}
                          aria-busy={stopping}
                          disabled={stopping || cancelAllBusy}
                          onClick={() => void stopShellTask(task)}
                        >
                          {stopping ? (
                            <IconLoader
                              size={14}
                              className="summary-panel__spin"
                            />
                          ) : (
                            <IconStopFilled size={14} />
                          )}
                        </Button>
                      </div>
                    );
                  })}
                </div>
                {shellTaskError ? (
                  <div className="summary-panel__notice is-error" role="alert">
                    <IconAlertTriangle size={14} />
                    <span>{shellTaskError}</span>
                  </div>
                ) : null}
              </section>
            ) : null}

            {subagents.length > 0 ? (
              <section className="summary-panel__agent-summary">
                {git || shellTasks.length > 0 || backgroundTasks.length > 0 ? (
                  <div className="summary-panel__divider" />
                ) : null}
                <div className="summary-panel__section-title">
                  {tr("summary.subagents.title")}
                </div>
                <div className="summary-panel__active-agents">
                  {activeSubagents.length ? (
                    activeSubagents.map((agent) => {
                      const task =
                        agent.status === "running"
                          ? agentTaskByThreadId.get(agent.agent_id)
                          : undefined;
                      const stopping = task
                        ? stoppingAgentTaskIds.has(task.taskId)
                        : false;
                      return (
                        <div
                          key={agent.agent_id}
                          className="summary-panel__agent-entry"
                        >
                          <SubagentRow
                            agent={agent}
                            locale={locale}
                            now={now}
                            onClick={() => {
                              onOpenSubagent(agent.agent_id);
                              onClose();
                            }}
                          />
                          {task ? (
                            <Button
                              type="button"
                              className="summary-panel__agent-stop"
                              aria-label={tr("summary.subagents.stop", {
                                name: agent.task_title || agent.agent_name,
                              })}
                              aria-busy={stopping}
                              disabled={stopping || cancelAllBusy}
                              onClick={() => void stopAgentTask(task)}
                            >
                              {stopping ? (
                                <IconLoader
                                  size={14}
                                  className="summary-panel__spin"
                                />
                              ) : (
                                <IconStopFilled size={14} />
                              )}
                            </Button>
                          ) : null}
                        </div>
                      );
                    })
                  ) : (
                    <div className="summary-panel__empty">
                      {tr("summary.subagents.noneRunning")}
                    </div>
                  )}
                </div>
                {agentTaskError ? (
                  <div className="summary-panel__notice is-error" role="alert">
                    <IconAlertTriangle size={14} />
                    <span>{agentTaskError}</span>
                  </div>
                ) : null}
                {groupedSubagents.done.length ? (
                  <Button
                    type="button"
                    className="summary-panel__completed"
                    onClick={() => {
                      onOpenSubagentList();
                      onClose();
                    }}
                  >
                    <span
                      className="summary-panel__avatar-group"
                      aria-hidden="true"
                    >
                      {groupedSubagents.done.slice(0, 3).map((agent) => (
                        <span key={agent.agent_id}>
                          <AgentAvatar
                            nickname={agent.nickname}
                            agentId={agent.agent_id}
                            size={24}
                            status="done"
                          />
                        </span>
                      ))}
                      {groupedSubagents.done.length > 3 ? (
                        <span className="summary-panel__avatar-more">
                          +{groupedSubagents.done.length - 3}
                        </span>
                      ) : null}
                    </span>
                    <span>
                      {tr("summary.subagents.completed", {
                        count: String(groupedSubagents.done.length),
                      })}
                    </span>
                    <IconChevronRight size={16} />
                  </Button>
                ) : null}
              </section>
            ) : null}
            {!git &&
            subagents.length === 0 &&
            shellTasks.length === 0 &&
            backgroundTasks.length === 0 &&
            !cancelAllFeedback &&
            !backgroundTaskError ? (
              <div className="summary-panel__empty summary-panel__empty--panel">
                {tr("summary.empty")}
              </div>
            ) : null}
          </div>
      </div>
      <GlassModal
        open={createBranchOpen}
        title={tr("summary.branches.createTitle")}
        size="sm"
        overlayClassName="summary-panel__branch-surface"
        closeLabel={tr("common.close")}
        onClose={() => !branchBusy && setCreateBranchOpen(false)}
        footer={
          <>
            <Button type="button" onClick={() => setCreateBranchOpen(false)}>
              {tr("common.cancel")}
            </Button>
            <Button
              type="button"
              disabled={newBranchInvalid || Boolean(branchBusy)}
              onClick={() => void checkoutBranch(newBranchName.trim(), true)}
            >
              {tr("summary.branches.createAction")}
            </Button>
          </>
        }
      >
        <Label className="summary-panel__create-branch-field">
          <span>{tr("summary.branches.name")}</span>
          <Input
            data-modal-autofocus
            value={newBranchName}
            maxLength={256}
            aria-invalid={newBranchInvalid}
            onChange={(event) => {
              setNewBranchName(event.target.value);
              setBranchError(null);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !newBranchInvalid) {
                event.preventDefault();
                void checkoutBranch(newBranchName.trim(), true);
              }
            }}
          />
          {newBranchInvalid ? (
            <small role="alert">{tr("summary.branches.invalid")}</small>
          ) : branchError ? (
            <small role="alert">{branchError}</small>
          ) : null}
        </Label>
      </GlassModal>
      {/* Portal 弹层需标记为摘要面板内部，避免捕获阶段外点击关闭面板。 */}
      <GlassModal
        open={cancelAllOpen}
        title={tr("summary.backgroundTasks.stopAllTitle")}
        size="sm"
        overlayClassName="summary-panel__background-tasks-surface"
        closeLabel={tr("common.close")}
        showClose={!cancelAllBusy}
        onClose={() => !cancelAllBusy && setCancelAllOpen(false)}
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              disabled={cancelAllBusy}
              onClick={() => setCancelAllOpen(false)}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              type="button"
              className="btn btn--danger"
              disabled={
                cancelAllBusy ||
                stoppingShellTaskIds.size > 0 ||
                stoppingAgentTaskIds.size > 0 ||
                individualStopBusyCount > 0 ||
                backgroundTasks.length === 0
              }
              onClick={() => void stopAllBackgroundTasks()}
            >
              {cancelAllBusy
                ? tr("summary.backgroundTasks.stopping")
                : tr("summary.backgroundTasks.stopAll")}
            </Button>
          </>
        }
      >
        <p>
          {tr("summary.backgroundTasks.stopAllConfirm", {
            count: String(backgroundTasks.length),
          })}
        </p>
      </GlassModal>
    </aside>
  );
}

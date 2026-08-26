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
  IconPush,
  IconStopFilled,
  IconTerminal,
} from "@/components/icons";
import { createT, type Locale } from "@/i18n";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import * as api from "@/lib/api";
import { Checkbox } from "@/components/ui/checkbox";
import { SubagentRow } from "@/components/SubagentRow";
import { AgentAvatar } from "@/components/AgentAvatar";

type GitAction = "commit" | "commit-push" | "push";

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

function errorMessage(value: unknown): string {
  if (value instanceof Error) return value.message;
  return typeof value === "string" ? value : String(value);
}

/** 判断一次文档点击是否发生在任务摘要面板以外。 */
export function shouldCloseConversationSummaryPanel(
  panel: Pick<HTMLElement, "contains"> | null,
  trigger: Pick<HTMLElement, "contains"> | null,
  target: EventTarget | null,
): boolean {
  if (!panel || !target) return false;
  const targetNode = target as Node;
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
  const [commitMessage, setCommitMessage] = useState("");
  const [includeUnstaged, setIncludeUnstaged] = useState(true);
  const [gitAction, setGitAction] = useState<GitAction | null>(null);
  const [gitFeedback, setGitFeedback] = useState<{
    kind: "success" | "error";
    message: string;
  } | null>(null);
  const [shellTasks, setShellTasks] = useState<api.BackgroundTaskInfo[]>([]);
  const [stoppingShellTaskIds, setStoppingShellTaskIds] = useState<Set<string>>(
    () => new Set(),
  );
  const [shellTaskError, setShellTaskError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const gitRequest = useRef(0);
  const shellTaskRequest = useRef(0);
  const gitActionRef = useRef<GitAction | null>(null);
  const previousSessionState = useRef(sessionState);
  const panelRef = useRef<HTMLElement>(null);

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

  const refreshShellTasks = useCallback(async (preserveError = false) => {
    const request = ++shellTaskRequest.current;
    if (!sessionId || !api.isTauri()) {
      setShellTasks([]);
      if (!preserveError) setShellTaskError(null);
      return;
    }
    try {
      const tasks = summaryShellTasks(
        await api.backgroundTasksList(),
        sessionId,
      );
      if (request !== shellTaskRequest.current) return;
      setShellTasks(tasks);
      if (!preserveError) setShellTaskError(null);
    } catch (error) {
      if (request !== shellTaskRequest.current) return;
      if (!preserveError) setShellTaskError(errorMessage(error));
    }
  }, [sessionId]);

  useEffect(() => {
    if (!open) {
      gitRequest.current += 1;
      shellTaskRequest.current += 1;
      return;
    }
    setGitFeedback(null);
    void refreshGit();
    void refreshShellTasks();
  }, [open, refreshGit, refreshShellTasks]);

  useEffect(() => {
    if (!open) return;
    const timer = window.setInterval(() => void refreshShellTasks(), 1_000);
    return () => window.clearInterval(timer);
  }, [open, refreshShellTasks]);

  useEffect(() => {
    const previous = previousSessionState.current;
    previousSessionState.current = sessionState;
    if (open && previous === "streaming" && sessionState !== "streaming") {
      void refreshGit(true);
    }
  }, [open, refreshGit, sessionState]);

  useEffect(() => {
    setGitFormOpen(false);
    setGitFeedback(null);
    setShellTasks([]);
    setStoppingShellTaskIds(new Set());
    setShellTaskError(null);
  }, [projectPath, sessionId]);

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
      if (event.key === "Escape") onClose();
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

  const stopShellTask = async (task: api.BackgroundTaskInfo) => {
    setStoppingShellTaskIds((current) => new Set(current).add(task.taskId));
    setShellTaskError(null);
    try {
      await api.backgroundTaskCancel(task.sessionId, task.taskId);
      await refreshShellTasks();
    } catch (error) {
      setShellTaskError(errorMessage(error));
      await refreshShellTasks(true);
    } finally {
      setStoppingShellTaskIds((current) => {
        const next = new Set(current);
        next.delete(task.taskId);
        return next;
      });
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

                <div className="summary-panel__row summary-panel__row--static">
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
                </div>

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

            {shellTasks.length > 0 ? (
              <section
                className="summary-panel__shells"
                aria-labelledby="summary-panel-shells-title"
              >
                {git ? <div className="summary-panel__divider" /> : null}
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
                          disabled={stopping}
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
                {git || shellTasks.length > 0 ? (
                  <div className="summary-panel__divider" />
                ) : null}
                <div className="summary-panel__section-title">
                  {tr("summary.subagents.title")}
                </div>
                <div className="summary-panel__active-agents">
                  {activeSubagents.length ? (
                    activeSubagents.map((agent) => (
                      <SubagentRow
                        key={agent.agent_id}
                        agent={agent}
                        locale={locale}
                        now={now}
                        onClick={() => {
                          onOpenSubagent(agent.agent_id);
                          onClose();
                        }}
                      />
                    ))
                  ) : (
                    <div className="summary-panel__empty">
                      {tr("summary.subagents.noneRunning")}
                    </div>
                  )}
                </div>
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
            {!git && subagents.length === 0 && shellTasks.length === 0 ? (
              <div className="summary-panel__empty summary-panel__empty--panel">
                {tr("summary.empty")}
              </div>
            ) : null}
          </div>
      </div>
    </aside>
  );
}

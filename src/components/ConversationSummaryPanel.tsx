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
  IconArrowLeft,
  IconBolt,
  IconCheck,
  IconChevronRight,
  IconClose,
  IconFileDiff,
  IconGitBranch,
  IconGitCommit,
  IconLoader,
  IconPush,
  IconSummary,
  IconSubagent,
  IconStop,
  IconTerminal,
} from "@/components/icons";
import { createT, type Locale } from "@/i18n";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import type { MessageSegment } from "@/lib/session";
import * as api from "@/lib/api";

type SummaryView = "overview" | "subagents" | "subagent-detail";
type GitAction = "commit" | "commit-push" | "push";

const TOOL_DETAIL_LIMIT = 4_000;
const SUBAGENT_EXCERPT_LIMIT = 110;

function errorMessage(value: unknown): string {
  if (value instanceof Error) return value.message;
  return typeof value === "string" ? value : String(value);
}

/** 子 Agent 列表使用的单行摘要，优先展示最终正文。 */
export function subagentExcerpt(agent: AcpSubagentInfo): string {
  const content = agent.segments.find(
    (segment): segment is Extract<MessageSegment, { kind: "content" }> =>
      segment.kind === "content" && Boolean(segment.text.trim()),
  );
  const thought = agent.segments.find(
    (segment): segment is Extract<MessageSegment, { kind: "thought" }> =>
      segment.kind === "thought" && Boolean(segment.text.trim()),
  );
  const tool = agent.segments.find(
    (segment): segment is Extract<MessageSegment, { kind: "tool" }> =>
      segment.kind === "tool",
  );
  const raw =
    content?.text.trim() ||
    agent.result?.trim() ||
    thought?.text.trim() ||
    tool?.title.trim() ||
    "";
  const flat = raw.replace(/\s+/g, " ");
  return flat.length > SUBAGENT_EXCERPT_LIMIT
    ? `${flat.slice(0, SUBAGENT_EXCERPT_LIMIT)}…`
    : flat;
}

/** 详情页避免把超大工具输出完整挂进 DOM。 */
export function compactToolDetail(value?: string): string {
  if (!value) return "";
  return value.length > TOOL_DETAIL_LIMIT
    ? `${value.slice(0, TOOL_DETAIL_LIMIT)}\n…`
    : value;
}

/** 只有已经结束且带稳定子线程标识的子 Agent 才能继续。 */
export function canResumeSubagent(agent: AcpSubagentInfo): boolean {
  return agent.status !== "running" && agent.agent_id.trim().length > 0;
}

/** 把 Peri 后台任务类别映射为固定的界面文案键。 */
export function backgroundTaskKindMessageKey(
  kind: api.BackgroundTaskKind,
):
  | "backgroundTasks.kind.shell"
  | "backgroundTasks.kind.agent"
  | "backgroundTasks.kind.workflow" {
  switch (kind) {
    case "agent":
      return "backgroundTasks.kind.agent";
    case "workflow":
      return "backgroundTasks.kind.workflow";
    default:
      return "backgroundTasks.kind.shell";
  }
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

/** 为不同后台任务类别选择现有的轻量图标。 */
function backgroundTaskIcon(kind: api.BackgroundTaskKind) {
  if (kind === "agent") return <IconSubagent size={17} />;
  if (kind === "workflow") return <IconBolt size={17} />;
  return <IconTerminal size={17} />;
}

function formatDuration(durationMs: number, locale: Locale): string {
  const seconds = Math.max(1, Math.floor(Math.max(0, durationMs) / 1_000));
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  if (locale === "en") {
    return minutes > 0 ? `${minutes}m ${rest}s` : `${seconds}s`;
  }
  return minutes > 0 ? `${minutes}分${rest}秒` : `${seconds}秒`;
}

function statusIcon(agent: AcpSubagentInfo) {
  if (agent.status === "running") {
    return <IconLoader size={15} className="summary-panel__spin" />;
  }
  if (agent.status === "failed") return <IconAlertTriangle size={15} />;
  return <IconCheck size={15} />;
}

function SubagentTimeline({
  segments,
  labels,
}: {
  segments: MessageSegment[];
  labels: {
    thought: string;
    tool: string;
    input: string;
    output: string;
    noActivity: string;
    statuses: Record<string, string>;
  };
}) {
  if (segments.length === 0) {
    return <div className="summary-panel__empty">{labels.noActivity}</div>;
  }
  return (
    <div className="summary-panel__timeline">
      {segments.map((segment, index) => {
        if (segment.kind === "thought") {
          return (
            <section
              className="summary-panel__timeline-block summary-panel__timeline-block--thought"
              key={`thought-${index}`}
            >
              <span className="summary-panel__timeline-label">
                {labels.thought}
              </span>
              <p>{segment.text}</p>
            </section>
          );
        }
        if (segment.kind === "content") {
          return (
            <section
              className="summary-panel__timeline-block summary-panel__timeline-block--content"
              key={`content-${index}`}
            >
              <p>{segment.text}</p>
            </section>
          );
        }
        const input = compactToolDetail(segment.input);
        const output = compactToolDetail(segment.output);
        return (
          <section
            className={
              "summary-panel__tool" +
              (segment.isError || segment.status === "failed"
                ? " is-error"
                : "")
            }
            key={segment.toolCallId || `tool-${index}`}
          >
            <div className="summary-panel__tool-head">
              <span className="summary-panel__tool-icon">
                {segment.streaming ? (
                  <IconLoader size={14} className="summary-panel__spin" />
                ) : segment.isError || segment.status === "failed" ? (
                  <IconAlertTriangle size={14} />
                ) : (
                  <IconCheck size={14} />
                )}
              </span>
              <span className="summary-panel__tool-title">
                {segment.title || labels.tool}
              </span>
              <span className="summary-panel__tool-status">
                {labels.statuses[segment.status] || segment.status}
              </span>
            </div>
            {input || output ? (
              <details className="summary-panel__tool-details">
                <summary>{labels.tool}</summary>
                {input ? (
                  <div>
                    <span>{labels.input}</span>
                    <pre>{input}</pre>
                  </div>
                ) : null}
                {output ? (
                  <div>
                    <span>{labels.output}</span>
                    <pre>{output}</pre>
                  </div>
                ) : null}
              </details>
            ) : null}
          </section>
        );
      })}
    </div>
  );
}

export interface ConversationSummaryPanelProps {
  /** 是否显示任务摘要面板。 */
  open: boolean;
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
  /** 向主 Agent 发送带稳定 child_thread_id 的继续请求。 */
  onResumeSubagent: (agentId: string, agentName: string) => Promise<boolean>;
}

export function ConversationSummaryPanel({
  open,
  triggerRef,
  projectPath,
  sessionId,
  sessionState,
  subagents,
  locale,
  onClose,
  onOpenChanges,
  onResumeSubagent,
}: ConversationSummaryPanelProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [view, setView] = useState<SummaryView>("overview");
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [git, setGit] = useState<api.GitStatusResult | null>(null);
  const [gitLoading, setGitLoading] = useState(false);
  const [gitError, setGitError] = useState<string | null>(null);
  const [gitFormOpen, setGitFormOpen] = useState(false);
  const [commitMessage, setCommitMessage] = useState("");
  const [includeUnstaged, setIncludeUnstaged] = useState(true);
  const [gitAction, setGitAction] = useState<GitAction | null>(null);
  const [gitFeedback, setGitFeedback] = useState<{
    kind: "success" | "error";
    message: string;
  } | null>(null);
  const [backgroundTasks, setBackgroundTasks] = useState<
    api.BackgroundTaskInfo[]
  >([]);
  const [cancellingTaskIds, setCancellingTaskIds] = useState<Set<string>>(
    () => new Set(),
  );
  const [backgroundTaskError, setBackgroundTaskError] = useState<string | null>(
    null,
  );
  const [resumingAgentId, setResumingAgentId] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const gitRequest = useRef(0);
  const gitActionRef = useRef<GitAction | null>(null);
  const previousSessionState = useRef(sessionState);
  const panelRef = useRef<HTMLElement>(null);

  const refreshGit = useCallback(async () => {
    const request = ++gitRequest.current;
    if (!projectPath) {
      setGit(null);
      setGitError(tr("summary.git.needProject"));
      setGitLoading(false);
      return;
    }
    if (!api.isTauri()) {
      setGit(null);
      setGitError(tr("summary.git.desktopOnly"));
      setGitLoading(false);
      return;
    }
    setGitLoading(true);
    setGitError(null);
    try {
      const result = await api.gitStatus(projectPath);
      if (request !== gitRequest.current) return;
      setGit(result);
      if (!result.available) {
        setGitError(result.reason || tr("summary.git.unavailable"));
      }
    } catch (error) {
      if (request !== gitRequest.current) return;
      setGit(null);
      setGitError(errorMessage(error));
    } finally {
      if (request === gitRequest.current) setGitLoading(false);
    }
  }, [projectPath, tr]);

  const refreshBackgroundTasks = useCallback(async (preserveError = false) => {
    if (!api.isTauri()) {
      setBackgroundTasks([]);
      if (!preserveError) setBackgroundTaskError(null);
      return;
    }
    try {
      setBackgroundTasks(await api.backgroundTasksList());
      if (!preserveError) setBackgroundTaskError(null);
    } catch (error) {
      setBackgroundTasks([]);
      if (!preserveError) setBackgroundTaskError(errorMessage(error));
    }
  }, []);

  useEffect(() => {
    if (!open) {
      gitRequest.current += 1;
      return;
    }
    setGitFeedback(null);
    void refreshGit();
    void refreshBackgroundTasks();
  }, [open, refreshBackgroundTasks, refreshGit]);

  useEffect(() => {
    if (!open) return;
    const timer = window.setInterval(
      () => void refreshBackgroundTasks(),
      1_000,
    );
    return () => window.clearInterval(timer);
  }, [open, refreshBackgroundTasks]);

  useEffect(() => {
    const previous = previousSessionState.current;
    previousSessionState.current = sessionState;
    if (open && previous === "streaming" && sessionState !== "streaming") {
      void refreshGit();
    }
  }, [open, refreshGit, sessionState]);

  useEffect(() => {
    setView("overview");
    setSelectedAgentId(null);
    setResumingAgentId(null);
    setGitFormOpen(false);
    setGitFeedback(null);
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
    document.addEventListener("pointerdown", onDocumentPointerDown, true);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onDocumentPointerDown, true);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [onClose, open, triggerRef]);

  const hasRunningAgent = subagents.some((agent) => agent.status === "running");
  useEffect(() => {
    if (!open || !hasRunningAgent) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [hasRunningAgent, open]);

  const orderedSubagents = useMemo(
    () =>
      [...subagents].sort((left, right) => {
        if (left.status === "running" && right.status !== "running") return -1;
        if (right.status === "running" && left.status !== "running") return 1;
        return right.started_at - left.started_at;
      }),
    [subagents],
  );
  const selectedAgent = subagents.find(
    (agent) => agent.agent_id === selectedAgentId,
  );
  const runningCount = subagents.filter(
    (agent) => agent.status === "running",
  ).length;

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

  const cancelBackgroundTask = async (task: api.BackgroundTaskInfo) => {
    setCancellingTaskIds((current) => new Set(current).add(task.taskId));
    setBackgroundTaskError(null);
    try {
      await api.backgroundTaskCancel(task.sessionId, task.taskId);
      await refreshBackgroundTasks();
    } catch (error) {
      await refreshBackgroundTasks(true);
      setBackgroundTaskError(errorMessage(error));
    } finally {
      setCancellingTaskIds((current) => {
        const next = new Set(current);
        next.delete(task.taskId);
        return next;
      });
    }
  };

  const cancelAllBackgroundTasks = async () => {
    setCancellingTaskIds(
      new Set(backgroundTasks.map((task) => task.taskId)),
    );
    setBackgroundTaskError(null);
    try {
      await api.backgroundTasksCancelAll();
      await refreshBackgroundTasks();
    } catch (error) {
      await refreshBackgroundTasks(true);
      setBackgroundTaskError(errorMessage(error));
    } finally {
      setCancellingTaskIds(new Set());
    }
  };

  /** 请求主 Agent 继续选中的持久化子线程，并阻止重复点击。 */
  const resumeSubagent = async (agent: AcpSubagentInfo) => {
    if (!canResumeSubagent(agent) || resumingAgentId) return;
    setResumingAgentId(agent.agent_id);
    try {
      await onResumeSubagent(agent.agent_id, agent.agent_name);
    } finally {
      setResumingAgentId(null);
    }
  };

  if (!open) return null;

  const panelTitle =
    view === "overview"
      ? tr("summary.title")
      : view === "subagents"
        ? tr("summary.subagents.title")
        : selectedAgent?.agent_name || tr("summary.subagents.title");

  return (
    <aside
      ref={panelRef}
      className="summary-panel"
      aria-label={tr("summary.title")}
    >
      <header className="summary-panel__header">
        {view !== "overview" ? (
          <button
            type="button"
            className="summary-panel__icon-btn"
            aria-label={tr("summary.back")}
            onClick={() => {
              if (view === "subagent-detail") {
                setView("subagents");
                setSelectedAgentId(null);
              } else {
                setView("overview");
              }
            }}
          >
            <IconArrowLeft size={17} />
          </button>
        ) : (
          <span className="summary-panel__header-icon">
            <IconSummary size={17} />
          </span>
        )}
        <strong className="summary-panel__title">{panelTitle}</strong>
        <button
          type="button"
          className="summary-panel__icon-btn"
          aria-label={tr("common.close")}
          onClick={onClose}
        >
          <IconClose size={17} />
        </button>
      </header>

      <div className="summary-panel__body">
        {view === "overview" ? (
          <>
            <button
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
                {git?.files.length ? (
                  <small>
                    {tr("summary.filesChanged", {
                      count: String(git.files.length),
                    })}
                  </small>
                ) : null}
              </span>
              <span className="summary-panel__diff-stat">
                <span className="is-addition">+{git?.additions ?? 0}</span>
                <span className="is-deletion">−{git?.deletions ?? 0}</span>
              </span>
            </button>

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

            <button
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
            </button>

            {gitFormOpen ? (
              <section className="summary-panel__git-form">
                <label className="summary-panel__commit-field">
                  <span className="sr-only">
                    {tr("summary.git.message")}
                  </span>
                  <input
                    value={commitMessage}
                    maxLength={4_096}
                    placeholder={tr("summary.git.messagePlaceholder")}
                    onChange={(event) => setCommitMessage(event.target.value)}
                    onKeyDown={handleCommitKeyDown}
                  />
                </label>
                <label className="summary-panel__checkbox">
                  <input
                    type="checkbox"
                    checked={includeUnstaged}
                    disabled={!git?.hasUnstagedChanges || Boolean(gitAction)}
                    onChange={(event) => setIncludeUnstaged(event.target.checked)}
                  />
                  <span>{tr("summary.git.includeUnstaged")}</span>
                  <span className="summary-panel__diff-stat">
                    <span className="is-addition">+{git?.additions ?? 0}</span>
                    <span className="is-deletion">−{git?.deletions ?? 0}</span>
                  </span>
                </label>
                <div className="summary-panel__git-actions">
                  <button
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
                  </button>
                  <button
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
                  </button>
                  <button
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
                  </button>
                </div>
              </section>
            ) : null}

            {gitLoading ? (
              <div className="summary-panel__notice" role="status">
                <IconLoader size={14} className="summary-panel__spin" />
                {tr("summary.loading")}
              </div>
            ) : gitError ? (
              <div className="summary-panel__notice is-error" role="status">
                <IconAlertTriangle size={14} />
                <span>{gitError}</span>
              </div>
            ) : gitFeedback ? (
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

            {backgroundTasks.length > 0 || backgroundTaskError ? (
              <>
                <div className="summary-panel__divider" />
                <div className="summary-panel__section-head">
                  <div className="summary-panel__section-title">
                    {tr("backgroundTasks.title")}
                  </div>
                  {backgroundTasks.length > 0 ? (
                    <button
                      type="button"
                      className="summary-panel__section-action"
                      disabled={cancellingTaskIds.size > 0}
                      onClick={() => void cancelAllBackgroundTasks()}
                    >
                      {tr("backgroundTasks.cancelAll")}
                    </button>
                  ) : null}
                </div>
                {backgroundTaskError ? (
                  <div
                    className="summary-panel__feedback summary-panel__feedback--error"
                    role="alert"
                  >
                    <IconAlertTriangle size={14} />
                    <span>{backgroundTaskError}</span>
                  </div>
                ) : null}
                <div className="summary-panel__tasks">
                  {backgroundTasks.map((task) => (
                    <div
                      key={`${task.sessionId}:${task.taskId}`}
                      className="summary-panel__task-row"
                    >
                      <span className="summary-panel__row-icon">
                        {backgroundTaskIcon(task.kind)}
                      </span>
                      <span
                        className="summary-panel__task-copy"
                        title={task.summary}
                      >
                        <strong>{task.summary}</strong>
                        <small>
                          {tr(backgroundTaskKindMessageKey(task.kind))}
                        </small>
                      </span>
                      <button
                        type="button"
                        className="summary-panel__task-cancel"
                        aria-label={tr("backgroundTasks.cancel")}
                        disabled={cancellingTaskIds.has(task.taskId)}
                        onClick={() => void cancelBackgroundTask(task)}
                      >
                        <IconStop size={12} />
                      </button>
                    </div>
                  ))}
                </div>
              </>
            ) : null}

            {subagents.length > 0 ? (
              <>
                <div className="summary-panel__divider" />
                <div className="summary-panel__section-title">
                  {tr("summary.subagents.title")}
                </div>
                <button
                  type="button"
                  className="summary-panel__row summary-panel__row--subagents"
                  onClick={() => setView("subagents")}
                >
                  <span className="summary-panel__row-icon">
                    <IconSubagent size={18} />
                  </span>
                  <span className="summary-panel__row-label">
                    {runningCount > 0
                      ? tr("summary.subagents.runningCount", {
                          count: String(runningCount),
                        })
                      : tr("summary.subagents.totalCount", {
                          count: String(subagents.length),
                        })}
                  </span>
                  <IconChevronRight size={16} />
                </button>
              </>
            ) : null}
          </>
        ) : null}

        {view === "subagents" ? (
          <div className="summary-panel__subagents">
            <div className="summary-panel__list-heading">
              {tr("summary.subagents.opened", {
                count: String(subagents.length),
              })}
            </div>
            {orderedSubagents.length === 0 ? (
              <div className="summary-panel__empty">
                {tr("summary.subagents.empty")}
              </div>
            ) : (
              orderedSubagents.map((agent) => {
                const end = agent.stopped_at ?? now;
                return (
                  <button
                    type="button"
                    className="summary-panel__agent-row"
                    key={agent.agent_id}
                    onClick={() => {
                      setSelectedAgentId(agent.agent_id);
                      setView("subagent-detail");
                    }}
                  >
                    <span
                      className={`summary-panel__agent-avatar is-${agent.status}`}
                    >
                      <IconSubagent size={18} />
                    </span>
                    <span className="summary-panel__agent-copy">
                      <strong>{agent.agent_name}</strong>
                      <small>
                        {subagentExcerpt(agent) ||
                          tr(
                            agent.status === "running"
                              ? "summary.subagents.processing"
                              : "summary.subagents.noActivity",
                          )}
                      </small>
                    </span>
                    <span className="summary-panel__agent-meta">
                      {formatDuration(end - agent.started_at, locale)}
                      <span className={`is-${agent.status}`}>
                        {statusIcon(agent)}
                      </span>
                    </span>
                  </button>
                );
              })
            )}
          </div>
        ) : null}

        {view === "subagent-detail" && selectedAgent ? (
          <div className="summary-panel__agent-detail">
            <div className="summary-panel__agent-status-line">
              <span className={`is-${selectedAgent.status}`}>
                {statusIcon(selectedAgent)}
              </span>
              <strong>
                {selectedAgent.status === "running"
                  ? tr("summary.subagents.processing")
                  : selectedAgent.status === "failed"
                    ? tr("summary.subagents.failed")
                    : tr("summary.subagents.processed")}
              </strong>
              <span>
                {formatDuration(
                  (selectedAgent.stopped_at ?? now) - selectedAgent.started_at,
                  locale,
                )}
              </span>
              {selectedAgent.is_background ? (
                <span className="summary-panel__background-tag">
                  {tr("summary.subagents.background")}
                </span>
              ) : null}
            </div>
            <code className="summary-panel__agent-id">
              {selectedAgent.agent_id}
            </code>
            {canResumeSubagent(selectedAgent) ? (
              <button
                type="button"
                className="summary-panel__resume-agent"
                disabled={resumingAgentId !== null}
                onClick={() => void resumeSubagent(selectedAgent)}
              >
                {resumingAgentId === selectedAgent.agent_id ? (
                  <IconLoader size={14} className="summary-panel__spin" />
                ) : (
                  <IconSubagent size={14} />
                )}
                <span>
                  {resumingAgentId === selectedAgent.agent_id
                    ? tr("summary.subagents.resuming")
                    : tr("summary.subagents.resume")}
                </span>
              </button>
            ) : null}
            <div className="summary-panel__divider" />
            <SubagentTimeline
              segments={selectedAgent.segments}
              labels={{
                thought: tr("summary.subagents.thought"),
                tool: tr("summary.subagents.toolDetail"),
                input: tr("summary.subagents.toolInput"),
                output: tr("summary.subagents.toolOutput"),
                noActivity: tr("summary.subagents.noActivity"),
                statuses: {
                  pending: tr("summary.subagents.toolPending"),
                  in_progress: tr("summary.subagents.toolRunning"),
                  completed: tr("summary.subagents.toolCompleted"),
                  failed: tr("summary.subagents.toolFailed"),
                },
              }}
            />
            {selectedAgent.result?.trim() &&
            !selectedAgent.segments.some(
              (segment) => segment.kind === "content" && segment.text.trim(),
            ) ? (
              <section className="summary-panel__timeline-block summary-panel__timeline-block--content">
                <p>{selectedAgent.result}</p>
              </section>
            ) : null}
          </div>
        ) : null}

        {view === "subagent-detail" && !selectedAgent ? (
          <div className="summary-panel__empty">
            {tr("summary.subagents.noActivity")}
          </div>
        ) : null}
      </div>
    </aside>
  );
}

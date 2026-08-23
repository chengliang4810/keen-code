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
  IconSummary,
} from "@/components/icons";
import { createT, type Locale } from "@/i18n";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import * as api from "@/lib/api";
import { Checkbox } from "@/components/ui/checkbox";
import { SubagentRow } from "@/components/SubagentRow";

type GitAction = "commit" | "commit-push" | "push";

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
  onOpenSubagent,
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
  const [now, setNow] = useState(() => Date.now());
  const gitRequest = useRef(0);
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

  useEffect(() => {
    if (!open) {
      gitRequest.current += 1;
      return;
    }
    setGitFeedback(null);
    void refreshGit();
  }, [open, refreshGit]);

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

  if (!open) return null;

  return (
    <aside
      ref={panelRef}
      className="summary-panel"
      aria-label={tr("summary.title")}
    >
      <header className="summary-panel__header">
        <span className="summary-panel__header-icon">
          <IconSummary size={17} />
        </span>
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
        <>
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

            {subagents.length > 0 ? (
              <>
                <div className="summary-panel__divider" />
                <div className="summary-panel__section-title">
                  {tr("summary.subagents.title")}
                </div>
                <div className="summary-panel__subagents">
                  <div className="summary-panel__list-heading">
                    {tr("summary.subagents.opened", {
                      count: String(subagents.length),
                    })}
                  </div>
                  {orderedSubagents.map((agent) => (
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
                  ))}
                </div>
              </>
            ) : null}
        </>
      </div>
    </aside>
  );
}

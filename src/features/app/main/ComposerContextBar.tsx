import type { Locale, MessageKey, Vars } from "@/i18n";
import type { Project } from "@/features/app/models";
import type { AcpSessionView } from "@/lib/acp/store";
import type { SessionSnapshot } from "@/lib/session";
import type { GitWorktreeEntry } from "@/lib/api";
import { ComposerTodoProgress } from "@/components/ComposerTodoProgress";
import {
  ComposerGoalProgress,
} from "@/components/ComposerGoalProgress";
import { ComposerProjectMenu } from "@/components/ComposerProjectMenu";
import { ComposerWorktreeMenu } from "@/components/ComposerWorktreeMenu";

type Translator = (key: MessageKey, vars?: Vars) => string;

/** Goal 状态栏使用的动作集合，避免壳层逐项转发 Composer controller。 */
interface ComposerGoalActions {
  /** 打开当前 Goal 的编辑弹窗。 */
  editCurrentGoal: () => void;
  /** 打开确认弹窗并清除当前 Goal。 */
  confirmClearCurrentGoal: () => void;
  /** 打开确认弹窗并将当前 active Goal 标记为完成。 */
  completeCurrentGoal: () => void;
  /** 打开原因输入并将当前 active Goal 标记为阻塞。 */
  blockCurrentGoal: () => void;
  /** 当前 Session 当前 Goal 是否正在提交状态转换。 */
  goalTransitionPending: boolean;
}

export interface ComposerContextBarProps {
  locale: Locale;
  tr: Translator;
  session: SessionSnapshot;
  activeProject: Project | null;
  projects: Project[];
  acpSessionView: AcpSessionView | null;
  welcomeSession: boolean;
  bindSessionProject: (
    project: Project | null,
    options?: { silent?: boolean },
  ) => Promise<void>;
  openAddProject: (
    intent: { bindSession: boolean },
    returnFocus?: HTMLElement | null,
  ) => void | Promise<void>;
  gitWorktrees: GitWorktreeEntry[];
  gitWorktreesAvailable: boolean | null;
  gitWorktreesLoading: boolean;
  gitWorktreesReason: string | null;
  switchToWorktree: (worktree: GitWorktreeEntry) => Promise<void>;
  openWorktreeCreate: (options?: { startNewChat?: boolean }) => void;
  openWorktreeGc: () => void;
  refreshGitWorktrees: () => Promise<void>;
  /** 当前 Goal 状态栏需要的编辑、清除、终态转换与 pending 动作。 */
  goalActions: ComposerGoalActions;
}

export function ComposerContextBar({
  locale,
  tr,
  session,
  activeProject,
  projects,
  acpSessionView,
  welcomeSession,
  bindSessionProject,
  openAddProject,
  gitWorktrees,
  gitWorktreesAvailable,
  gitWorktreesLoading,
  gitWorktreesReason,
  switchToWorktree,
  openWorktreeCreate,
  openWorktreeGc,
  refreshGitWorktrees,
  goalActions,
}: ComposerContextBarProps) {
  return (
    <>
      <ComposerTodoProgress
        key={`composer-todo-${session.sessionId ?? "draft"}`}
        locale={locale}
        todos={
          acpSessionView?.session_id === session.sessionId
            ? acpSessionView.todos
            : null
        }
      />
      <ComposerGoalProgress
        locale={locale}
        goal={
          acpSessionView?.session_id === session.sessionId
            ? acpSessionView.goal
            : null
        }
        onEdit={goalActions.editCurrentGoal}
        onClear={goalActions.confirmClearCurrentGoal}
        onComplete={goalActions.completeCurrentGoal}
        onBlock={goalActions.blockCurrentGoal}
        goalTransitionPending={goalActions.goalTransitionPending}
        running={session.state === "streaming"}
      />
      {welcomeSession ? (
        <div
          className="composer__context-bar"
          aria-label={tr("composer.pickProject")}
        >
          <ComposerProjectMenu
            activeProject={activeProject}
            projects={projects}
            labels={{
              pickProject: tr("composer.pickProject"),
              addProject: tr("composer.addProject"),
              pathMissing: tr("project.pathMissingShort"),
            }}
            disabled={session.state === "streaming"}
            onSelect={(project) => {
              void bindSessionProject(project);
            }}
            onAdd={(returnFocus) => {
              openAddProject({ bindSession: true }, returnFocus);
            }}
          />
          {activeProject && gitWorktreesAvailable === true ? (
            <ComposerWorktreeMenu
              variant="context"
              activePath={activeProject.path}
              worktrees={gitWorktrees}
              worktreesAvailable={gitWorktreesAvailable}
              worktreesLoading={gitWorktreesLoading}
              worktreesReason={gitWorktreesReason}
              disabled={session.state === "streaming"}
              labels={{
                worktrees: tr("composer.worktrees"),
                worktreesEmpty: tr("composer.worktreesEmpty"),
                worktreesUnavailable: tr("composer.worktreesUnavailable"),
                worktreesLoading: tr("composer.worktreesLoading"),
                worktreeCurrent: tr("composer.worktreeCurrent"),
                worktreeMain: tr("composer.worktreeMain"),
                worktreeDetached: tr("composer.worktreeDetached"),
                worktreeTip: tr("composer.worktreeTip"),
                worktreeNew: tr("composer.worktreeNew"),
                worktreeNewChat: tr("composer.worktreeNewChat"),
                worktreeGc: tr("composer.worktreeGc"),
              }}
              onSwitch={(worktree) => {
                void switchToWorktree(worktree);
              }}
              onCreate={() => openWorktreeCreate()}
              onCreateAndChat={() => openWorktreeCreate({ startNewChat: true })}
              onGc={openWorktreeGc}
              onOpen={refreshGitWorktrees}
            />
          ) : null}
        </div>
      ) : null}
    </>
  );
}

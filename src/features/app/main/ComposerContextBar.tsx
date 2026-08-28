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
  editCurrentGoal: () => void;
  confirmClearCurrentGoal: () => void;
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
  editCurrentGoal,
  confirmClearCurrentGoal,
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
        onEdit={editCurrentGoal}
        onClear={confirmClearCurrentGoal}
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

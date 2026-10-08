import { WorkspaceInputBar } from "@/app/components/WorkspaceInputBar";
import { Spinner } from "@/components/localized/spinner";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogTitle,
} from "@/components/ui/dialog";
import { AiChatView } from "@/modules/ai/components/AiChat";
import { ConversationFiles } from "@/modules/ai/components/ConversationFiles";
import { ConversationStatusPanel } from "@/modules/ai/components/ConversationStatusPanel";
import { NewConversationPage } from "@/modules/ai/components/NewConversationPage";
import { StorageLoadError } from "@/modules/ai/components/StorageLoadError";
import { useSpaces } from "@/modules/spaces/lib/useSpaces";
import type { ConversationPanelMode } from "@/modules/ai/lib/conversationLayout";
import {
  createConversationStatusSelector,
  workspaceGitStatus,
} from "@/modules/ai/lib/conversationPresentation";
import type { GitStatusSnapshot } from "@/modules/ai/lib/native";
import type { Todo } from "@/modules/ai/lib/todos";
import { getOrCreateChat } from "@/modules/ai/store/chatRuntime";
import { useChatStore } from "@/modules/ai/store/chatStore";
import { usePlanStore } from "@/modules/ai/store/planStore";
import { useTodosStore } from "@/modules/ai/store/todoStore";
import { useTranslation } from "@/modules/i18n";
import { type UIMessage, useChat } from "@ai-sdk/react";
import { Cancel01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from "react";

const EMPTY_TODOS: Todo[] = [];
const PlanDiffReview = lazy(() =>
  import("@/modules/ai/components/PlanDiffReview").then((module) => ({
    default: module.PlanDiffReview,
  })),
);

type Props = {
  workspaceRoot: string | null;
  home: string | null;
  hasComposer: boolean;
  keysLoaded: boolean;
  toolsOpen: boolean;
  gitStatus?: GitStatusSnapshot | null;
  onOpenTerminal: () => void;
  onOpenFiles: () => void;
  onOpenFile?: (path: string) => void;
  onOpenGit: () => void;
  onConnect: () => void;
  onSelectProject: (id: string | null) => void;
  onNewProject: () => void;
};

export function AgentWorkbench(props: Props) {
  const tr = useTranslation();
  const sessionId = useChatStore((s) => s.activeSessionId);
  const draftSession = useChatStore((s) => s.draftSession);
  const sessionsError = useChatStore((s) => s.sessionsError);
  const sessionsLoading = useChatStore((s) => s.sessionsLoading);
  const sessionsReady = useChatStore((s) => s.sessionsHydrated);
  const hydrateSessions = useChatStore((s) => s.hydrateSessions);
  const projectsError = useSpaces((s) => s.loadError);
  const projectsLoading = useSpaces((s) => s.loading);
  const projectsReady = useSpaces((s) => s.hydrated);
  const retryProjects = useSpaces((s) => s.retryLoad);
  const storageError = sessionsError ?? projectsError;
  return (
    <section
      className="@container/conversation relative flex h-full min-h-0 min-w-0 flex-col"
      data-agent-workbench
      aria-label={tr("Agent workbench")}
    >
      {storageError ? (
        <div className="flex flex-1 items-center justify-center">
          <StorageLoadError
            error={storageError}
            loading={sessionsError ? sessionsLoading : projectsLoading}
            onRetry={
              sessionsError
                ? () => {
                    void hydrateSessions();
                  }
                : retryProjects
            }
          />
        </div>
      ) : sessionsReady &&
        projectsReady &&
        draftSession &&
        draftSession.id === sessionId ? (
        <NewConversationPage {...props} />
      ) : sessionsReady && projectsReady && sessionId ? (
        <TaskBody key={sessionId} sessionId={sessionId} {...props} />
      ) : (
        <div className="flex flex-1 items-center justify-center gap-2 text-ui-sm text-muted-foreground">
          <Spinner className="size-3" />
          {tr("Loading sessions…")}
        </div>
      )}
    </section>
  );
}

function TaskBody({
  sessionId,
  workspaceRoot,
  home,
  hasComposer,
  keysLoaded,
  gitStatus,
  onOpenTerminal,
  onOpenFiles,
  onOpenFile,
  onOpenGit,
  onConnect,
}: Props & { sessionId: string }) {
  const tr = useTranslation();
  const session = useChatStore((s) =>
    s.sessions.find((task) => task.id === sessionId),
  );
  const chat = useMemo(() => getOrCreateChat(sessionId), [sessionId]);
  // 一帧内的文本增量合并发布，避免高频 token 使 Markdown 重复工作。
  const helpers = useChat<UIMessage>({ chat, experimental_throttle: 16 });
  const title =
    !session?.title || session.title === "New chat"
      ? tr("New task")
      : session.title;
  const hydrateTodos = useTodosStore((s) => s.hydrate);
  const todos = useTodosStore((s) => s.bySession[sessionId]) ?? EMPTY_TODOS;
  const [panelMode, setPanelMode] = useState<ConversationPanelMode>("auto");
  const planReviewCount = usePlanStore((s) => s.queue.length);
  const [planReviewOpen, setPlanReviewOpen] = useState(false);
  const selectStatus = useMemo(() => createConversationStatusSelector(), []);
  const model = useMemo(
    () => selectStatus(helpers.messages),
    [selectStatus, helpers.messages],
  );
  const git = workspaceGitStatus(workspaceRoot, gitStatus);
  const busy = helpers.status === "submitted" || helpers.status === "streaming";
  const hasStatus = !!(
    git ||
    todos.length ||
    planReviewCount ||
    model.files.length ||
    model.terminals.length ||
    model.agents.length
  );
  const stop = useCallback(() => {
    void helpers.stop();
  }, [helpers.stop]);
  const regenerate = useCallback(
    () => helpers.regenerate(),
    [helpers.regenerate],
  );

  useEffect(() => {
    void hydrateTodos(sessionId);
  }, [sessionId, hydrateTodos]);

  useEffect(() => {
    if (!planReviewCount) setPlanReviewOpen(false);
  }, [planReviewCount]);

  return (
    <ConversationFiles workspaceRoot={workspaceRoot} onOpenFile={onOpenFile}>
      <h1 className="sr-only">{title}</h1>
      <ConversationStatusPanel
        model={model}
        todos={todos}
        gitStatus={git}
        mode={panelMode}
        onModeChange={setPanelMode}
        busy={busy}
        onStop={stop}
        onOpenFiles={onOpenFiles}
        onOpenGit={onOpenGit}
        onOpenTerminal={onOpenTerminal}
        planReviewCount={planReviewCount}
        onReviewPlan={() => setPlanReviewOpen(true)}
      />
      {planReviewOpen && planReviewCount > 0 && (
        <Dialog open onOpenChange={setPlanReviewOpen}>
          <DialogContent
            className="h-[min(80dvh,42rem)] overflow-hidden p-0 sm:max-w-3xl"
            aria-describedby={undefined}
            showCloseButton={false}
          >
            <DialogTitle className="sr-only">{tr("Plan review")}</DialogTitle>
            {/* 审查内容使用绝对定位，关闭控件保留独立的点击层。 */}
            <DialogClose asChild>
              <Button
                variant="ghost"
                size="icon-sm"
                className="absolute right-3 top-2 z-20"
                aria-label={tr("Close plan review")}
              >
                <HugeiconsIcon icon={Cancel01Icon} size={16} />
              </Button>
            </DialogClose>
            <div className="relative h-full min-h-0 pt-12">
              <Suspense
                fallback={
                  <div className="flex h-full items-center justify-center">
                    <Spinner />
                  </div>
                }
              >
                <div className="relative h-full">
                  <PlanDiffReview />
                </div>
              </Suspense>
            </div>
          </DialogContent>
        </Dialog>
      )}
      <AiChatView
        messages={helpers.messages}
        status={helpers.status}
        error={helpers.error}
        clearError={helpers.clearError}
        addToolApprovalResponse={helpers.addToolApprovalResponse}
        stop={helpers.stop}
        onRegenerate={regenerate}
        panelOffset={hasStatus && panelMode !== "mini"}
        emptyState={
          <div className="w-full max-w-lg">
            <h2 className="text-lg font-medium tracking-tight">
              {tr("What would you like to build?")}
            </h2>
            <p className="mt-2 text-ui-sm leading-relaxed text-muted-foreground">
              {tr(
                "Describe a development task. The agent will inspect the project, implement changes, and ask you to review actions.",
              )}
            </p>
            <p
              className="mt-4 truncate text-ui-sm text-muted-foreground/70"
              title={workspaceRoot ?? undefined}
            >
              {workspaceRoot}
            </p>
          </div>
        }
        bottomDock={
          <WorkspaceInputBar
            agentWorkbench
            isBlockTab={false}
            isTerminalTab={false}
            activeLeafId={null}
            cwd={workspaceRoot}
            home={home}
            hasComposer={hasComposer}
            panelOpen
            keysLoaded={keysLoaded}
            onConnect={onConnect}
          />
        }
      />
    </ConversationFiles>
  );
}

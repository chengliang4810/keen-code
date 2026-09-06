import type {
  Dispatch,
  MutableRefObject,
  RefObject,
  SetStateAction,
} from "react";
import type { Locale } from "@/i18n";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import type { Attachment } from "@/lib/attachments";
import type { ResourceOpenTarget } from "@/components/ResourceViewer";
import type { ChatMessage, SessionSnapshot } from "@/lib/session";
import type { LayoutPrefs } from "@/lib/layout";
import type { ChatFindActive } from "@/hooks/useChatFind";
import type { RetryStatus } from "@/hooks/useSessionTurn";
import type { ComposerController } from "@/hooks/useComposerController";
import type { Project } from "@/features/app/models";
import { ConversationSummaryPanel } from "@/components/ConversationSummaryPanel";
import { ConversationThread } from "@/components/lobe-chat/ConversationThread";
import { saveLayout } from "@/lib/layout";
import { mergeAttachments } from "@/lib/attachments";

type SetState<T> = Dispatch<SetStateAction<T>>;

export interface ConversationStageProps {
  locale: Locale;
  messages: ChatMessage[];
  session: SessionSnapshot;
  activeProject: Project | null;
  showWelcomeCopy: boolean;
  turnStartedAt: number | null;
  retryStatus: RetryStatus | null;
  layout: LayoutPrefs;
  setLayout: SetState<LayoutPrefs>;
  setResourceOpenTarget: SetState<ResourceOpenTarget | null>;
  setAttachments: SetState<Attachment[]>;
  editAndResendLastUserMessage: (
    message: ChatMessage,
    content: string,
  ) => Promise<boolean>;
  attachLabels: ComposerController["attachmentLabels"];
  showChatFind: boolean;
  chatFindQuery: string;
  chatFindHitIds: ReadonlySet<string>;
  chatFindActive: ChatFindActive;
  handleFirstVisibleToken: (turnId: string) => void;
  activeTurnIdBySessionRef: MutableRefObject<Map<string, string>>;
  displayedSubagents: AcpSubagentInfo[];
  summaryOpen: boolean;
  summaryTriggerRef: RefObject<HTMLButtonElement | null>;
  closeSummary: () => void;
}

export function ConversationStage({
  locale,
  messages,
  session,
  activeProject,
  showWelcomeCopy,
  turnStartedAt,
  retryStatus,
  layout,
  setLayout,
  setResourceOpenTarget,
  setAttachments,
  editAndResendLastUserMessage,
  attachLabels,
  showChatFind,
  chatFindQuery,
  chatFindHitIds,
  chatFindActive,
  handleFirstVisibleToken,
  activeTurnIdBySessionRef,
  displayedSubagents,
  summaryOpen,
  summaryTriggerRef,
  closeSummary,
}: ConversationStageProps) {
  const revealAside = () => {
    setLayout((current) => {
      if (!current.asideCollapsed) return current;
      const next = { ...current, asideCollapsed: false };
      saveLayout(localStorage, next);
      return next;
    });
  };

  return (
    <>
      <ConversationThread
        locale={locale}
        messages={messages}
        sessionState={session.state}
        sessionKey={session.sessionId ?? `draft-${session.title ?? "new"}`}
        projectPath={activeProject?.path ?? null}
        turnStartedAt={turnStartedAt}
        retryStatus={retryStatus}
        suppressEmptyCopy={!showWelcomeCopy}
        onOpenSessionChanges={() => {
          revealAside();
          setResourceOpenTarget({ type: "changes" });
        }}
        onOpenModifiedPath={(path) => {
          revealAside();
          setResourceOpenTarget({ type: "changes", path });
        }}
        onOpenResource={(target) => {
          revealAside();
          setResourceOpenTarget(target);
        }}
        onAddAttachmentToComposer={(attachment) =>
          setAttachments((previous) => mergeAttachments(previous, [attachment]))
        }
        onEditLastUserMessage={editAndResendLastUserMessage}
        attachLabels={attachLabels}
        findQuery={showChatFind ? chatFindQuery : ""}
        findHitMessageIds={showChatFind ? chatFindHitIds : undefined}
        findActive={showChatFind ? chatFindActive : null}
        onFirstVisibleToken={handleFirstVisibleToken}
        activeTurnId={
          session.sessionId
            ? activeTurnIdBySessionRef.current.get(session.sessionId)
            : undefined
        }
        subagents={displayedSubagents}
      />

      <ConversationSummaryPanel
        open={summaryOpen}
        dismissOnOutsidePress={!layout.asideCollapsed}
        triggerRef={summaryTriggerRef}
        projectPath={activeProject?.path ?? null}
        sessionId={session.sessionId}
        sessionState={session.state}
        subagents={displayedSubagents}
        locale={locale}
        onClose={closeSummary}
        onOpenSubagent={(agentId) => {
          revealAside();
          setResourceOpenTarget({ type: "subagent", agentId });
        }}
        onOpenSubagentList={() => {
          revealAside();
          setResourceOpenTarget({ type: "subagents" });
        }}
        onOpenChanges={() => {
          revealAside();
          setResourceOpenTarget({ type: "changes" });
        }}
      />
    </>
  );
}

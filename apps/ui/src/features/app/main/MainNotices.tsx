import type {
  Dispatch,
  SetStateAction,
} from "react";
import type { MessageKey, Vars } from "@/i18n";
import type { Project } from "@/features/app/models";
import type {
  ErrorBannerView,
  SessionSnapshot,
} from "@/lib/session";
import type { StreamStallState } from "@/hooks/useSessionTurn";
import type { SessionLiveMap } from "@/lib/sessionLiveStore";
import { Button } from "@appica/ui-react/button";
import {
  Alert,
  AlertAction,
  AlertDescription,
  AlertTitle,
} from "@appica/ui-react/alert";
import { ChatFindBar } from "@/components/ChatFindBar";
import { isProjectPathMissing } from "@/lib/projectPath";
import {
  normalizeStallTier,
  stallMessageKey,
  stallTierFromProgress,
} from "@/lib/sessionPhase";

type SetState<T> = Dispatch<SetStateAction<T>>;
type Translator = (key: MessageKey, vars?: Vars) => string;
type EnsureConnected = (
  forceOrOptions?: boolean | { force?: boolean; sessionId?: string | null },
) => Promise<string | null>;

export interface MainNoticesProps {
  tr: Translator;
  activeProject: Project | null;
  /** Web Host 项目是只读投影，不能显示本地目录重定位入口。 */
  canWriteProjects: boolean;
  relocateProject: (project: Project) => void | Promise<void>;
  emptyExistingSession: boolean;
  streamStall: StreamStallState | null;
  liveMap: SessionLiveMap;
  session: SessionSnapshot;
  setStreamStall: SetState<StreamStallState | null>;
  stop: () => Promise<void>;
  showChatFind: boolean;
  chatFindFocusKey: number;
  chatFindQuery: string;
  chatFindIndex: number;
  chatFindMatches: Array<{
    index: number;
    messageId: string;
    occurrence: number;
    start: number;
    end: number;
  }>;
  chatFindPrev: () => void;
  chatFindNext: () => void;
  setChatFindQuery: SetState<string>;
  setChatFindIndex: SetState<number>;
  setShowChatFind: SetState<boolean>;
  errorBanner: ErrorBannerView | null;
  hasChatTurnError: boolean;
  errorDetailOpen: boolean;
  setErrorDetailOpen: SetState<boolean>;
  connecting: boolean;
  runErrorBannerAction: (
    action: NonNullable<ErrorBannerView["primary"]>,
  ) => void;
  ensureConnected: EnsureConnected;
  setLocalError: SetState<string | null>;
}

export function MainNotices({
  tr,
  activeProject,
  canWriteProjects,
  relocateProject,
  emptyExistingSession,
  streamStall,
  liveMap,
  session,
  setStreamStall,
  stop,
  showChatFind,
  chatFindFocusKey,
  chatFindQuery,
  chatFindIndex,
  chatFindMatches,
  chatFindPrev,
  chatFindNext,
  setChatFindQuery,
  setChatFindIndex,
  setShowChatFind,
  errorBanner,
  hasChatTurnError,
  errorDetailOpen,
  setErrorDetailOpen,
  connecting,
  runErrorBannerAction,
  ensureConnected,
  setLocalError,
}: MainNoticesProps) {
  const stallTier = streamStall
    ? (() => {
        const sid = streamStall.sessionId || session.sessionId || "";
        const live = liveMap[sid];
        const saw =
          !!streamStall.sawModelOutput || !!live?.sawModelOutput || false;
        const tools =
          !!streamStall.sawToolActivity || !!live?.sawToolActivity || false;
        return (
          normalizeStallTier(streamStall.tier) ??
          stallTierFromProgress({
            sawModelOutput: saw,
            sawToolActivity: tools,
            terminalCandidate: saw && !live?.liveToolId,
          })
        );
      })()
    : null;
  const stallMessage = streamStall
    ? (() => {
        const key = stallMessageKey(stallTier!);
        if (key === "endOfTurn.stallPreToken") {
          return tr("endOfTurn.stallPreToken");
        }
        if (key === "endOfTurn.stallWorkingTools") {
          return tr("endOfTurn.stallWorkingTools");
        }
        if (key === "endOfTurn.stallMaybeDone") {
          return tr("endOfTurn.stallMaybeDone");
        }
        return tr("error.deck.stall.problem");
      })()
    : null;

  return (
    <>
      {canWriteProjects && activeProject && isProjectPathMissing(activeProject.pathOk) && (
        <Alert variant="warning" layout="inline" className="mx-5 mb-2">
          <AlertDescription>
            {tr("project.pathMissingShort")}
          </AlertDescription>
          <AlertAction>
            <Button
              type="button"
              variant="primary"
              size="md"
              onClick={() => void relocateProject(activeProject)}
            >
              {tr("project.relocateToSend")}
            </Button>
          </AlertAction>
        </Alert>
      )}

      {emptyExistingSession && (
        <Alert className="mx-5 mb-2">
          <AlertDescription>{tr("session.empty")}</AlertDescription>
        </Alert>
      )}

      {streamStall && stallTier && stallMessage ? (
        <Alert
          className="mx-5 mb-2"
          variant={
            stallTier === "maybe_done" || stallTier === "post_output"
              ? "warning"
              : "error"
          }
        >
          <AlertTitle>STREAM_STALL: {stallMessage}</AlertTitle>
          <AlertDescription>
            {tr("error.deck.stall.cause", {
              seconds: String(streamStall.stallSeconds),
            })}
          </AlertDescription>
          <AlertAction>
            <Button
              type="button"
              variant="ghost"
              size="md"
              onClick={() => setStreamStall(null)}
            >
              {tr("agent.streamStallKeepWaiting")}
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="md"
              onClick={() => {
                setStreamStall(null);
                void stop();
              }}
            >
              {tr("agent.streamStallEndTurn")}
            </Button>
          </AlertAction>
        </Alert>
      ) : null}

      {showChatFind && (
        <ChatFindBar
          key={chatFindFocusKey}
          query={chatFindQuery}
          activeIndex={chatFindIndex}
          matchCount={chatFindMatches.length}
          labels={{
            placeholder: tr("chatFind.placeholder"),
            prev: tr("chatFind.prev"),
            next: tr("chatFind.next"),
            close: tr("chatFind.close"),
            count: tr("chatFind.count"),
            noMatches: tr("chatFind.noMatches"),
            aria: tr("chatFind.aria"),
          }}
          onQueryChange={(query) => {
            setChatFindQuery(query);
            setChatFindIndex(0);
          }}
          onPrev={chatFindPrev}
          onNext={chatFindNext}
          onClose={() => setShowChatFind(false)}
        />
      )}

      {errorBanner && !hasChatTurnError && (
        <Alert variant="error" className="mx-5 mb-2">
          <AlertTitle>
            {errorBanner.code ? `${errorBanner.code}: ` : ""}
            {errorBanner.summary}
          </AlertTitle>
          {errorBanner.cause ? (
            <AlertDescription>{errorBanner.cause}</AlertDescription>
          ) : null}
          <AlertAction>
            {errorBanner.primary ? (
              <Button
                type="button"
                variant="primary"
                size="md"
                disabled={
                  connecting && errorBanner.primary.id === "reconnect"
                }
                onClick={() => runErrorBannerAction(errorBanner.primary!)}
              >
                {errorBanner.primary.label}
              </Button>
            ) : null}
            {errorBanner.secondary ? (
              <Button
                type="button"
                variant="ghost"
                size="md"
                disabled={
                  connecting && errorBanner.secondary.id === "reconnect"
                }
                onClick={() => runErrorBannerAction(errorBanner.secondary!)}
              >
                {errorBanner.secondary.label}
              </Button>
            ) : null}
            {!errorBanner.primary &&
              (errorBanner.reconnectHint || session.state === "disconnected") ? (
              <Button
                type="button"
                variant="ghost"
                size="md"
                disabled={connecting}
                onClick={() => {
                  setLocalError(null);
                  setErrorDetailOpen(false);
                  void ensureConnected(true).then((sessionId) => {
                    if (sessionId) setLocalError(null);
                  });
                }}
              >
                {tr("main.reconnect")}
              </Button>
            ) : null}
            {errorBanner.detail ? (
              <Button
                type="button"
                variant="ghost"
                size="md"
                aria-expanded={errorDetailOpen}
                onClick={() => setErrorDetailOpen((value) => !value)}
              >
                {errorDetailOpen
                  ? tr("error.hideDetails")
                  : tr("error.details")}
              </Button>
            ) : null}
          </AlertAction>
          {errorBanner.detail && errorDetailOpen ? (
            <AlertDescription>
              <pre className="max-h-40 overflow-auto whitespace-pre-wrap font-mono">
                {errorBanner.detail}
              </pre>
            </AlertDescription>
          ) : null}
        </Alert>
      )}
    </>
  );
}

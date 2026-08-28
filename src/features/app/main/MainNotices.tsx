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
import { Button } from "@/components/ui/button";
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
      {activeProject && isProjectPathMissing(activeProject.pathOk) && (
        <div className="conn-bar">
          <span style={{ fontSize: 12, opacity: 0.9, marginRight: 8 }}>
            {tr("project.pathMissingShort")}
          </span>
          <Button
            type="button"
            className="btn btn--primary"
            style={{ height: 24, fontSize: 11 }}
            onClick={() => void relocateProject(activeProject)}
          >
            {tr("project.relocateToSend")}
          </Button>
        </div>
      )}

      {emptyExistingSession && (
        <div className="conn-bar" role="status">
          <span style={{ fontSize: 12, opacity: 0.85 }}>
            {tr("session.empty")}
          </span>
        </div>
      )}

      {streamStall && stallTier && stallMessage ? (
        <div
          className={
            `stall-banner error-banner${
              stallTier === "maybe_done" || stallTier === "post_output"
                ? " stall-banner--soft"
                : ""
            }`
          }
          role="status"
        >
          <div className="error-banner__code">STREAM_STALL</div>
          <div className="error-banner__summary">{stallMessage}</div>
          <div className="error-banner__cause">
            {tr("error.deck.stall.cause", {
              seconds: String(streamStall.stallSeconds),
            })}
          </div>
          <div className="stall-banner__actions error-banner__actions">
            <Button
              type="button"
              className="btn btn--primary stall-banner__btn"
              onClick={() => setStreamStall(null)}
            >
              {tr("agent.streamStallKeepWaiting")}
            </Button>
            <Button
              type="button"
              className="btn btn--ghost stall-banner__btn"
              onClick={() => {
                setStreamStall(null);
                void stop();
              }}
            >
              {tr("agent.streamStallEndTurn")}
            </Button>
          </div>
        </div>
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
        <div className="error-banner" role="alert">
          {errorBanner.code ? (
            <div className="error-banner__code">{errorBanner.code}</div>
          ) : null}
          <div className="error-banner__summary">{errorBanner.summary}</div>
          {errorBanner.cause ? (
            <div className="error-banner__cause">{errorBanner.cause}</div>
          ) : null}
          <div className="error-banner__actions">
            {errorBanner.primary ? (
              <Button
                type="button"
                className="btn btn--primary error-banner__primary"
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
                className="btn btn--ghost error-banner__secondary"
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
                className="btn btn--ghost error-banner__reconnect"
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
                className="error-banner__details-btn"
                aria-expanded={errorDetailOpen}
                onClick={() => setErrorDetailOpen((value) => !value)}
              >
                {errorDetailOpen
                  ? tr("error.hideDetails")
                  : tr("error.details")}
              </Button>
            ) : null}
          </div>
          {errorBanner.detail && errorDetailOpen ? (
            <pre className="error-banner__detail">{errorBanner.detail}</pre>
          ) : null}
        </div>
      )}
    </>
  );
}

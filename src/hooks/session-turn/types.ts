import type {
  Dispatch,
  MutableRefObject,
  SetStateAction,
} from "react";
import type { Locale } from "@/i18n";
import type { Project } from "@/features/app/models";
import type { Attachment } from "@/lib/attachments";
import type {
  AskUserPayload,
  ChatMessage,
  SessionSnapshot,
} from "@/lib/session";
import type { AcpWorkspaceState } from "@/lib/acp/store";
import type { GoalRecordDto } from "@/lib/acp/events";
import type {
  SessionSendAccepted,
  SessionSnapshot as AcpSessionSnapshot,
} from "@/lib/acp/api";
import type { SessionLiveMap } from "@/lib/sessionLiveStore";
import type { SessionPreferencePatch } from "@/lib/sessionPreferences";
import type { TurnLatencyState } from "@/lib/turnLatency";
import type { ViewFocus } from "@/lib/viewFocus";

export type StateSetter<T> = Dispatch<SetStateAction<T>>;
export type Ref<T> = MutableRefObject<T>;

export type RetryStatus = {
  attempt: number;
  maxAttempts: number;
  reason: string;
};

export type StreamStallState = {
  sessionId?: string;
  stallSeconds: number;
  tier?: string;
  sawModelOutput?: boolean;
  sawToolActivity?: boolean;
};

export interface SessionTurnApiPort {
  isTauri: () => boolean;
  connect: (args: {
    projectPath?: string;
    sessionId?: string | null;
  }) => Promise<AcpSessionSnapshot>;
  setEffort: (args: { sessionId: string; effort: string }) => Promise<void>;
  send: (args: {
    text: string;
    sessionId: string;
    requestId: string;
    planMode?: boolean;
    ultraMode?: boolean;
  }) => Promise<SessionSendAccepted>;
  stop: (sessionId: string, requestId: string) => Promise<AcpSessionSnapshot>;
  steer: (args: { text: string; sessionId: string }) => Promise<void>;
  prepareEditLastUser: (args: {
    sessionId: string;
    expectedText: string;
  }) => Promise<{ archivedBranchId: string }>;
  goalUpsert: (args: {
    sessionId: string;
    goal: Partial<GoalRecordDto> & { title: string };
  }) => Promise<{
    revision: number;
    goal: GoalRecordDto;
    deduplicated: boolean;
  }>;
}

export interface SessionTurnRuntimePort {
  acpWorkspaceRef: Ref<AcpWorkspaceState>;
  liveHostRef: Ref<SessionSnapshot>;
  messagesBySessionRef: Ref<Map<string, ChatMessage[]>>;
  viewingSessionIdRef: Ref<string | null>;
  applyViewProjectionRef: Ref<(sessionId: string | null) => void>;
  commitWorkspace: () => void;
  patchSessionMessages: (
    sessionId: string | null | undefined,
    reduce: (previous: ChatMessage[]) => ChatMessage[],
  ) => void;
  currentViewFocus: () => ViewFocus;
  replayHistory: (sessionId: string, originView?: ViewFocus) => Promise<void>;
  refreshSessions: () => Promise<void>;
  applyMessagePrefixTitle: (sessionId: string, userText: string) => void;
  applyAutomaticSessionTitle: (
    sessionId: string,
    firstUserMessage: string,
    expectedTitle?: string | null,
  ) => Promise<void>;
  updateSessionPreference: (
    sessionId: string,
    patch: SessionPreferencePatch,
  ) => void;
}

export interface SessionTurnUiPort {
  setSession: StateSetter<SessionSnapshot>;
  setMessages: StateSetter<ChatMessage[]>;
  setLiveHost: StateSetter<SessionSnapshot>;
  setLiveMap: StateSetter<SessionLiveMap>;
  setRetryStatus: StateSetter<RetryStatus | null>;
  setTurnStartedAt: StateSetter<number | null>;
  setStreamStall: StateSetter<StreamStallState | null>;
  setLocalError: StateSetter<string | null>;
  setAskUser: StateSetter<AskUserPayload | null>;
  setDraft: StateSetter<string>;
  setAttachments: StateSetter<Attachment[]>;
  setGoalModeSessionKey: StateSetter<string | null>;
  setPlanModeSessionKey: StateSetter<string | null>;
  setUltraModeSessionKey: StateSetter<string | null>;
  promptHistoryIndexRef: Ref<number | null>;
  setPromptHistoryIndex: StateSetter<number | null>;
  setPromptHistoryOpen: StateSetter<boolean>;
  setPromptHistoryFilter: StateSetter<string>;
  setPromptHistoryActive: StateSetter<number>;
  setPromptHistoryFocusFilter: StateSetter<boolean>;
}

/** Refs shared with the ACP event reducer so sends and events converge on one turn state. */
export interface SessionTurnStateRefs {
  sendInFlightRef: Ref<boolean>;
  turnLatencyBySessionRef: Ref<Map<string, TurnLatencyState>>;
  activeTurnIdBySessionRef: Ref<Map<string, string>>;
  recoverableCompletedTurnIdBySessionRef: Ref<Map<string, string>>;
  completedTurnIdBySessionRef: Ref<Map<string, string>>;
  pendingVisibleTurnBySessionRef: Ref<Map<string, string>>;
  observeHostActiveTurn: (snapshot: {
    sessionId?: string | null;
    activeTurnId?: string | null;
  }) => void;
}

export interface UseSessionTurnOptions {
  locale: Locale;
  session: SessionSnapshot;
  activeProject: Project | null;
  draft: string;
  attachments: Attachment[];
  modelLabel: string;
  effort: string;
  hasConfiguredModel: boolean;
  goalModeSessionKey: string | null;
  planModeSessionKey: string | null;
  ultraModeSessionKey: string | null;
  api: SessionTurnApiPort;
  runtime: SessionTurnRuntimePort;
  ui: SessionTurnUiPort;
  /** Optional refs already owned by the ACP runtime. */
  stateRefs?: SessionTurnStateRefs;
  showToast: (message: string, durationMs?: number) => void;
  clearPendingAskUser: (sessionId?: string | null) => void;
}

export type EnsureConnected = (
  forceOrOptions?:
    | boolean
    | { force?: boolean; sessionId?: string | null },
) => Promise<string | null>;

export interface ExecuteSendOptions {
  storedDisplay: string;
  att: Attachment[];
  createGoal?: boolean;
  planMode?: boolean;
  ultraMode?: boolean;
  fromQueue?: boolean;
  targetSessionId?: string | null;
}

export type ExecuteSend = (options: ExecuteSendOptions) => Promise<boolean>;

export interface SessionTurnQueuePort {
  enqueue: (input: {
    storedDisplay: string;
    attachments: Attachment[];
    createGoal?: boolean;
    planMode?: boolean;
    ultraMode?: boolean;
  }) => unknown;
  releaseFlushHold: () => void;
  bindDraft: (sessionId: string) => void;
}

export interface SessionTurnState {
  connecting: boolean;
  connectingRef: Ref<boolean>;
  setConnectingState: (value: boolean) => void;
  sendInFlightRef: Ref<boolean>;
  turnLatencyBySessionRef: Ref<Map<string, TurnLatencyState>>;
  activeTurnIdBySessionRef: Ref<Map<string, string>>;
  recoverableCompletedTurnIdBySessionRef: Ref<Map<string, string>>;
  completedTurnIdBySessionRef: Ref<Map<string, string>>;
  pendingVisibleTurnBySessionRef: Ref<Map<string, string>>;
  observeHostActiveTurn: SessionTurnStateRefs["observeHostActiveTurn"];
}

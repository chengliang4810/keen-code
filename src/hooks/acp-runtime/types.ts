import type {
  Dispatch,
  MutableRefObject,
  SetStateAction,
} from "react";
import type { Locale } from "@/i18n";
import type {
  AskUserPayload,
  ChatMessage,
  SessionSnapshot,
} from "@/lib/session";
import type { SessionContextUsage, SessionRow } from "@/features/app/models";
import type { ModelOption } from "@/lib/modelCatalog";
import type * as api from "@/lib/api";
import type { AcpWorkspaceState } from "@/lib/acp/store";
import type { SessionLiveMap } from "@/lib/sessionLiveStore";
import type { TurnLatencyState } from "@/lib/turnLatency";
import type { ViewFocus } from "@/lib/viewFocus";

export type Ref<T> = MutableRefObject<T>;
export type SetState<T> = Dispatch<SetStateAction<T>>;
export type RetryStatus = {
  attempt: number;
  maxAttempts: number;
  delayMs: number;
  reason: string;
};
export type ClearPendingAskUser = (sessionId?: string | null, rpcId?: number) => void;
export type ViewProjection = (sessionId: string | null) => void;
export type SessionMessageReducer = (messages: ChatMessage[]) => ChatMessage[];

export interface UseAcpSessionRuntimeOptions {
  locale: Locale;
  session: SessionSnapshot;
  messages: ChatMessage[];
  liveHost: SessionSnapshot;
  acpWorkspace: AcpWorkspaceState;
  observeHostActiveTurn: (snapshot: {
    sessionId?: string | null;
    activeTurnId?: string | null;
  }) => void;
  commitWorkspace: () => void;
  acpWorkspaceRef: Ref<AcpWorkspaceState>;
  turnLatencyBySessionRef: Ref<Map<string, TurnLatencyState>>;
  activeTurnIdBySessionRef: Ref<Map<string, string>>;
  recoverableCompletedTurnIdBySessionRef: Ref<Map<string, string>>;
  completedTurnIdBySessionRef: Ref<Map<string, string>>;
  pendingVisibleTurnBySessionRef: Ref<Map<string, string>>;
  liveHostRef: Ref<SessionSnapshot>;
  messagesRef: Ref<ChatMessage[]>;
  messagesBySessionRef: Ref<Map<string, ChatMessage[]>>;
  modelBySessionRef: Ref<Map<string, string>>;
  contextUsageBySessionRef: Ref<Map<string, SessionContextUsage>>;
  taskCacheUsageRequestSeqRef: Ref<number>;
  viewingSessionIdRef: Ref<string | null>;
  openingSessionIdRef: Ref<string | null>;
  currentViewFocus: () => ViewFocus;
  sessionTitleOverridesRef: Ref<Map<string, string>>;
  sessionsRef: Ref<SessionRow[]>;
  sendInFlightRef: Ref<boolean>;
  configuredModelsRef: Ref<ModelOption[]>;
  clearPendingAskUserRef: Ref<ClearPendingAskUser>;
  pendingAskUserBySessionRef: Ref<Map<string, AskUserPayload>>;
  setPendingAskUserSessionIds: SetState<Set<string>>;
  setAskUser: SetState<AskUserPayload | null>;
  setSession: SetState<SessionSnapshot>;
  setMessages: SetState<ChatMessage[]>;
  setLiveHost: SetState<SessionSnapshot>;
  setLiveMap: SetState<SessionLiveMap>;
  setContextUsage: SetState<SessionContextUsage | null>;
  setTaskCacheUsage: SetState<api.TaskCacheUsage | null>;
  setRetryStatus: SetState<RetryStatus | null>;
  setTurnStartedAt: SetState<number | null>;
  setEffort: SetState<string>;
  setModelId: SetState<string>;
  promptHistoryIndexRef: Ref<number | null>;
  setPromptHistoryIndex: SetState<number | null>;
  setPromptHistoryOpen: SetState<boolean>;
  setPromptHistoryFilter: SetState<string>;
  setPromptHistoryActive: SetState<number>;
  setPromptHistoryFocusFilter: SetState<boolean>;
  setCompletedUnreadIds: SetState<Set<string>>;
}

export interface UseAcpSessionRuntimeResult {
  acpWorkspaceRef: Ref<AcpWorkspaceState>;
  acpWorkspace: AcpWorkspaceState;
  turnLatencyBySessionRef: Ref<Map<string, TurnLatencyState>>;
  activeTurnIdBySessionRef: Ref<Map<string, string>>;
  recoverableCompletedTurnIdBySessionRef: Ref<Map<string, string>>;
  completedTurnIdBySessionRef: Ref<Map<string, string>>;
  pendingVisibleTurnBySessionRef: Ref<Map<string, string>>;
  liveHostRef: Ref<SessionSnapshot>;
  messagesRef: Ref<ChatMessage[]>;
  messagesBySessionRef: Ref<Map<string, ChatMessage[]>>;
  contextUsageBySessionRef: Ref<Map<string, SessionContextUsage>>;
  taskCacheUsageRequestSeqRef: Ref<number>;
  refreshTaskCacheUsage: (sessionId: string | null) => Promise<void>;
  applyViewProjection: ViewProjection;
  applyViewProjectionRef: Ref<ViewProjection>;
  handleFirstVisibleToken: (turnId: string) => void;
  replayHistory: (sessionId: string, originView?: ViewFocus) => Promise<void>;
  patchSessionMessages: (
    targetSessionId: string | undefined | null,
    reduce: SessionMessageReducer,
  ) => void;
}

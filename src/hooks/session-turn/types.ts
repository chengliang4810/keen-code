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
  SessionPromptRun,
  SessionRewindResult,
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
  delayMs: number;
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
    /** 新建 Session 的确定性幂等标识。 */
    operationId: string;
  }) => Promise<AcpSessionSnapshot>;
  setEffort: (args: {
    /** 目标 Session。 */
    sessionId: string;
    /** 当前推理强度。 */
    effort: string;
    /** Journal 提交的幂等标识。 */
    operationId: string;
  }) => Promise<void>;
  send: (args: {
    text: string;
    sessionId: string;
    requestId: string;
    planMode?: boolean;
    ultraMode?: boolean;
  }) => SessionPromptRun;
  stop: (sessionId: string, requestId: string) => Promise<void>;
  steer: (args: {
    /** 引导消息正文。 */
    text: string;
    /** 当前运行中的 Session。 */
    sessionId: string;
    /** mailbox 写入的幂等标识。 */
    operationId: string;
  }) => Promise<void>;
  rewind: (args: {
    sessionId: string;
    /** 要删除的目标用户消息的后端稳定标识。 */
    targetMessageId: string;
    /** 目标用户消息的完整原始 Agent 文本，不做 trim。 */
    expectedText: string;
    /** 首版不自动恢复文件，固定为 false。 */
    revertFiles: false;
    /** rewind 事务的幂等标识，同时作为 JSON-RPC 请求 ID。 */
    operationId: string;
  }) => Promise<SessionRewindResult>;
  goalUpsert: (args: {
    /** 提供项目作用域的 Session 标识。 */
    sessionId: string;
    /** 用户可编辑的 Goal 字段。 */
    goal: {
      /** Goal 用户可见标题。 */
      title: string;
      /** 完整目标描述。 */
      objective: string;
      /** 可选补充说明。 */
      description?: string;
    };
    /** 当前投影修订号。 */
    expectedRevision: number;
    /** 本次变更的幂等标识。 */
    requestNonce: string;
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
  /** 显示与持久化的用户消息。 */
  storedDisplay: string;
  /** 本轮附件快照。 */
  att: Attachment[];
  /** 是否在发送前创建项目 Goal。 */
  createGoal?: boolean;
  /** 本轮 Plan 状态快照。 */
  planMode?: boolean;
  /** 本轮主动委派状态快照。 */
  ultraMode?: boolean;
  /** 是否来自可重试的 Session 队列。 */
  fromQueue?: boolean;
  /** 队列重试时复用的 Turn 标识；普通发送省略。 */
  requestId?: string;
  /** 明确发送到的 Session；空值表示新草稿。 */
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

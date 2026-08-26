import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Button } from "@/components/ui/button";
import {
  Suspense,
  lazy,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type DragEvent as ReactDragEvent,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { createPortal } from "react-dom";
import { useFloatingMenu } from "@/lib/floatingMenu";
import {
  applyNativeWindowTheme,
  applyThemePreference,
  applyThemeToDocument,
  getSystemTheme,
  loadThemePreference,
  resolveTheme,
  saveThemePreference,
  subscribeSystemTheme,
  type Theme,
  type ThemePreference,
} from "@/lib/theme";
import {
  applySkinToDocument,
  applyWallpaperFlag,
  applyWallpaperScrimToDocument,
  clearWallpaper,
  DEFAULT_WALLPAPER_FOCUS,
  loadSkin,
  loadWallpaperRecord,
  loadWallpaperScrim,
  saveSkin,
  saveWallpaper,
  saveWallpaperAdjust,
  saveWallpaperMediaSize,
  saveWallpaperScrim,
  skinPreferredTheme,
  type ThemeSkinId,
  type WallpaperClip,
  type WallpaperFocus,
  type WallpaperRecord,
} from "@/lib/themeSkin";
import { WallpaperMediaLayer } from "@/components/WallpaperMediaLayer";
import {
  ASIDE_WIDTH_MIN,
  DEFAULT_LAYOUT,
  SIDEBAR_WIDTH_MIN,
  clampAsideWidth,
  clampSidebarWidth,
  loadLayout,
  saveLayout,
  shouldCollapsePane,
} from "@/lib/layout";
import {
  hitDragZoneFromRects,
  toClientDragPoint,
  type DragZone,
} from "@/lib/dragZone";
import {
  applyTurnMarker,
  canType,
  clearPriorTurnErrors,
  clearPriorTurnStreaming,
  isSessionLiveStreaming,
  localizeUiError,
  presentErrorBanner,
  snapshotOutgoingMessages,
  type ErrorBannerView,
  IDLE_SNAPSHOT,
  type AskUserPayload,
  type ChatMessage,
  type SessionSnapshot,
} from "@/lib/session";
import {
  attachContextWindow,
  formatContextChipLabel,
} from "@/lib/contextUsage";
import { ContextUsageChip } from "@/components/ContextUsageChip";
import { ConversationSummaryPanel } from "@/components/ConversationSummaryPanel";
import * as api from "@/lib/api";
import { claimClipboardFiles } from "@/lib/clipboardPaste";
import {
  loadSessionOrder,
  moveId,
  orderedByIds,
  saveSessionOrder,
} from "@/lib/sidebarOrder";
import {
  collapsedIdsFromExpandMap,
  sameCollapsedIdSet,
} from "@/lib/sidebarExpand";
import { isGoalToolName } from "@/lib/toolDisplay";
import {
  armStopLatch,
  canSendWithStopLatch,
  canStopWithStopLatch,
  createStopLatchState,
  tickStopLatch,
  type StopLatchState,
  STOP_LATCH_MS,
} from "@/lib/stopLatch";
import {
  isSameView,
  isViewingSendTarget,
  shouldAdoptView,
  type ViewFocus,
} from "@/lib/viewFocus";
import {
  busySessionIds,
  projectHostIntoLiveMap,
  type SessionLiveMap,
} from "@/lib/sessionLiveStore";
import {
  createTurnLatencyState,
  reduceTurnLatency,
  summarizeTurnLatency,
  turnLatencyNow,
  turnUsageActionFromAcp,
  type TurnLatencyState,
} from "@/lib/turnLatency";
import {
  createActiveTurnBootstrapBuffer,
  resolveActiveTurnFromHostSnapshot,
} from "@/lib/activeTurn";
import { endOfTurnMarkerContent } from "@/lib/endOfTurn";
import {
  isNormalSessionCompletion,
  loadCompletedUnreadSessionIds,
  saveCompletedUnreadSessionIds,
} from "@/lib/sessionCompletion";
import {
  stallMessageKey,
  stallTierFromProgress,
  normalizeStallTier,
} from "@/lib/sessionPhase";
import { createT, type Locale } from "@/i18n";
import {
  applyModelMetadata,
  DEFAULT_EFFORT,
  effortsForModel,
  hasConfiguredProviderModel,
  isValidEffort,
  isValidModelId,
  pickDefaultEffort,
  pickNewChatModel,
  type ModelOption,
} from "@/lib/modelCatalog";
import { AskUserModal } from "@/components/AskUserModal";
import {
  filterSessionSearch,
} from "@/lib/sessionSearch";
import {
  sessionExportFilename,
  sessionToMarkdown,
} from "@/lib/sessionExport";
import {
  findChatMatches,
  stepChatFindIndex,
  type ChatFindMatch,
} from "@/lib/chatFind";
import { shortcutsForPlatform } from "@/lib/shortcuts";
import { GlassModal } from "@/components/GlassModal";
import { AppUpdateProgress } from "@/components/AppUpdateProgress";
import { appUpdateActionFor } from "@/lib/appUpdate";
import { ChatFindBar } from "@/components/ChatFindBar";
import {
  buildAgentPrompt,
  isImagePath,
  mergeAttachments,
  pathBasename,
  type Attachment,
} from "@/lib/attachments";
import {
  applySkillAtSlash,
  isDraftEmpty,
  detectSlashQueryFromEditor,
  parseStoredContent,
  serializeForAgent,
} from "@/lib/draftDoc";
import {
  collectUserPromptHistory,
  filterPromptHistory,
  shouldHandlePromptHistoryKey,
  stepPromptHistory,
  type PromptHistoryEntry,
} from "@/lib/composerPromptHistory";
import { PromptHistoryPanel } from "@/components/PromptHistoryPanel";
import {
  queuePreviewText,
  shouldEnqueueSend,
  type QueuedSend,
} from "@/lib/sendQueue";
import {
  useSendQueue,
  type ExecuteSendFromQueue,
} from "@/hooks/useSendQueue";
import {
  buildSlashCatalog,
  flattenFilteredCatalog,
  type SlashItem,
  type SkillInfo,
} from "@/lib/slashCatalog";
import type { MessageKey } from "@/i18n";
import { AttachmentCard } from "@/components/AttachmentCard";
import { ImageViewerProvider } from "@/components/ImageViewer";
import { OverlayScroll } from "@/components/OverlayScroll";
import { VirtualList } from "@/components/VirtualList";
import {
  SIDEBAR_SESSION_ROW_GAP,
  SIDEBAR_SESSION_ROW_HEIGHT,
} from "@/lib/virtualList";
import { StartupScreen } from "@/components/StartupScreen";
import {
  autoArchiveExpiredSessions,
  loadSessionPreferences,
  removeSessionPreference,
  updateSessionPreference,
} from "@/lib/sessionPreferences";
import {
  buildSessionTitleFromFirstMessage,
  canGenerateAutomaticSessionTitle,
  extractFirstUserMessageText,
  isPlaceholderSessionTitle,
  sanitizeGeneratedSessionTitle,
} from "@/lib/sessionTitle";
import {
  projectAcpConversation,
  projectAcpSnapshot,
  projectSidebar,
} from "@/lib/sessionProjection";
import {
  projectPeriStoredMessages,
  projectPeriStoredSubagentThreads,
  projectPeriStoredSubagents,
  withSubagentPrompts,
} from "@/lib/periStoredMessages";
import {
  beginLocalSessionTurn,
  createAcpWorkspaceState,
  reduceAgentEvent,
  reduceGoalSnapshot,
  reduceRecovery,
  reduceReplayResult,
  reduceSessionUpdate,
  resolveSessionUpdateSourceAgentId,
  type AcpWorkspaceState,
} from "@/lib/acp/store";
import {
  goalClear,
  goalUpsert,
  goalsList,
  listenAcp,
  sessionsList,
  sessionConnect,
  sessionDisconnect,
  sessionGetState,
  diagnosticsRecord,
  sessionFork,
  sessionPrepareEditLastUser,
  sessionMessages,
  sessionSubagents,
  sessionDelete,
  sessionGenerateTitle,
  sessionRename as acpSessionRename,
  sessionReplay,
  sessionResolveAskUser,
  sessionSend,
  sessionSteer,
  sessionSetEffort,
  sessionSetModel,
  sessionStop,
} from "@/lib/acp/api";
import {
  isForegroundRequestDone,
  isReplayedUpdate,
  isRequestScopedAgentEvent,
  isRequestScopedSessionUpdate,
  parseAgentEvent,
  shouldAcceptAgentDone,
  shouldDriveMainSessionStreaming,
  shouldApplyAgentEvent,
  shouldApplySessionUpdate,
} from "@/lib/acp/events";
import {
  commitLiveTurnToHistory,
  ensureAcpSession,
  reduceReplayedSessionUpdate,
  replaceHistoryTurnMetrics,
} from "@/lib/acp/projection";
import { createAnimationFrameBatcher } from "@/lib/frameBatcher";
import {
  ComposerEditor,
} from "@/components/ComposerEditor";
import { ComposerProjectMenu } from "@/components/ComposerProjectMenu";
import { ComposerWorktreeMenu } from "@/components/ComposerWorktreeMenu";
import {
  buildWorktreeSiblingPath,
  mainWorktreePath,
  pathsEqual,
  sanitizeWorktreeName,
} from "@/lib/gitWorktree";
import { isProjectPathMissing } from "@/lib/projectPath";
import {
  ComposerPlusPanel,
  buildComposerPlusEntries,
  uploadMatchesQuery,
} from "@/components/ComposerPlusPanel";
import { StatusModal } from "@/components/StatusModal";
import {
  IconChevronDown,
  IconMore,
  IconPlus,
  IconSearch,
  IconSkills,
  IconPuzzle,
  IconAttach,
  IconSend,
  IconStop,
  IconFolder,
  IconFolderOpen,
  IconFolderPlus,
  IconArrowsVerticalCollapse,
  IconClock,
  IconClose,
  IconNewChat as IconSquarePen,
  IconNewChat,
  IconPanel,
  IconPanelRight,
  IconSummary,
  IconArchive,
  IconPin,
  IconPinOff,
  IconRename,
  IconCopy,
  IconTrash,
  IconExternalLink,
  IconFork,
  IconListTree,
} from "@/components/icons";
import { ContextMenu, type ContextMenuItem } from "@/components/ContextMenu";
import {
  ComposerModelMenu,
} from "@/components/ComposerModelMenu";
import { ComposerReasoningMenu } from "@/components/ComposerReasoningMenu";
import {
  ResourceViewer,
  type ResourceOpenTarget,
} from "@/components/ResourceViewer";
import { ConversationThread } from "@/components/lobe-chat/ConversationThread";
import { ComposerTodoProgress } from "@/components/ComposerTodoProgress";
import {
  ComposerGoalChip,
  ComposerGoalProgress,
} from "@/components/ComposerGoalProgress";
import { ComposerPlanModeChip } from "@/components/ComposerPlanModeChip";
import { Spinner } from "@/components/ui/spinner";
import { Checkbox } from "@/components/ui/checkbox";
import { UserMenu } from "@/components/UserMenu";
import { type SettingsSectionId } from "@/components/SettingsPage";
const SettingsPage = lazy(() =>
  import("@/components/SettingsPage").then((module) => ({
    default: module.SettingsPage,
  })),
);
const settingsPageFallback = (
  <div className="settings-page settings-page--fallback" aria-busy="true" />
);
import {
  buildSettingsHash,
  parseSettingsHash,
} from "@/lib/settingsCatalog";
import { Tip } from "@/components/ui/tooltip";
import {
  WindowControls,
  toggleMaximizeFromTitlebar,
} from "@/components/WindowControls";

interface Project {
  id: string;
  name: string;
  path: string;
  pathOk: boolean;
}

interface SessionRow {
  id: string;
  title: string;
  projectId: string | null;
  updatedAt: string;
  archived: boolean;
  /** Pinned chats float to the top of the sidebar */
  pinned: boolean;
}

/** peri ThreadStore 当前持久化消息的最小结构。 */
/** 判断未知值是否为可安全读取字段的普通对象。 */
function isObjectRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function projectPathPreview(parent: string, name: string): string {
  const separator = parent.includes("\\") ? "\\" : "/";
  return `${parent.replace(/[/\\]+$/, "")}${separator}${name}`;
}

/** 从 ACP elicitation 事件读取可用于拒绝无效请求的数字请求标识。 */
function readElicitationRpcId(value: unknown): number | null {
  if (!isObjectRecord(value)) return null;
  const rpcId = value.rpcId;
  return typeof rpcId === "number" && Number.isSafeInteger(rpcId)
    ? rpcId
    : null;
}

/** 将当前 ACP form elicitation 契约严格转换为工作台问答载荷。 */
export function parseElicitationPayload(value: unknown): AskUserPayload | null {
  if (!isObjectRecord(value) || value.method !== "elicitation/create") {
    return null;
  }
  const rpcId = readElicitationRpcId(value);
  const params = value.params;
  if (rpcId == null || !isObjectRecord(params) || params.mode !== "form") {
    return null;
  }
  const sessionId =
    typeof params.sessionId === "string" ? params.sessionId.trim() : "";
  const schema = params.requestedSchema;
  if (!sessionId || !isObjectRecord(schema) || schema.type !== "object") {
    return null;
  }
  const properties = schema.properties;
  if (!isObjectRecord(properties)) return null;

  const questions: AskUserPayload["questions"] = [];
  for (const [id, rawProperty] of Object.entries(properties)) {
    if (!id || !isObjectRecord(rawProperty)) return null;
    const multiSelect = rawProperty.type === "array";
    if (!multiSelect && rawProperty.type !== "string") return null;

    let rawOptions: unknown;
    if (multiSelect) {
      const items = rawProperty.items;
      if (!isObjectRecord(items)) return null;
      rawOptions = items.anyOf;
    } else {
      rawOptions = rawProperty.oneOf;
    }
    if (rawOptions !== undefined && !Array.isArray(rawOptions)) return null;
    if (multiSelect && !Array.isArray(rawOptions)) return null;

    const options = (rawOptions ?? []).map((rawOption) => {
      if (!isObjectRecord(rawOption) || typeof rawOption.const !== "string") {
        return null;
      }
      const optionId = rawOption.const;
      const label =
        typeof rawOption.title === "string" && rawOption.title.trim()
          ? rawOption.title.trim()
          : optionId;
      const description =
        typeof rawOption.description === "string" &&
        rawOption.description.trim()
          ? rawOption.description.trim()
          : undefined;
      return {
        id: optionId,
        label,
        ...(description ? { description } : {}),
      };
    });
    if (options.some((option) => option == null)) return null;

    const question =
      (typeof rawProperty.description === "string"
        ? rawProperty.description.trim()
        : "") ||
      (typeof rawProperty.title === "string"
        ? rawProperty.title.trim()
        : "") ||
      (typeof params.message === "string" ? params.message.trim() : "") ||
      id;
    questions.push({
      id,
      question,
      options: options as AskUserPayload["questions"][number]["options"],
      ...(multiSelect ? { multiSelect: true } : {}),
    });
  }
  if (questions.length === 0) return null;
  return { rpcId, sessionId, questions };
}

/** 按问题标识把问答弹窗结果转换为 ACP schema 字段与选项值。 */
export function toElicitationAnswers(
  payload: AskUserPayload,
  modalAnswers: Record<string, string>,
): Record<string, string | string[]> {
  const answers: Record<string, string | string[]> = {};
  for (const question of payload.questions) {
    const rawAnswer = modalAnswers[question.id];
    if (rawAnswer == null) continue;
    const optionValue = (label: string) =>
      question.options.find((option) => option.label === label)?.id ?? label;
    if (question.multiSelect) {
      answers[question.id] = rawAnswer
        .split(", ")
        .map((answer) => optionValue(answer));
    } else {
      answers[question.id] = optionValue(rawAnswer);
    }
  }
  return answers;
}

/** 单个 ACP Session 最近一次上报的上下文使用量。 */
interface SessionContextUsage {
  /** 当前上下文已使用的 token 数。 */
  used: number;
  /** 当前模型的上下文容量。 */
  size: number;
  /** true 表示使用量来自本地请求体估算。 */
  estimated: boolean;
}

type ContextMenuState =
  | { kind: "project"; id: string; x: number; y: number }
  | { kind: "session"; id: string; x: number; y: number }
  | null;

/** In-app dialogs — window.prompt/confirm are unreliable in Tauri WebView. */
type AppDialog =
  | {
      kind: "confirm";
      title: string;
      message: string;
      confirmLabel?: string;
      danger?: boolean;
      onConfirm: () => void | Promise<void>;
    }
  | {
      kind: "prompt";
      title: string;
      initial: string;
      /** 输入框上方的可选补充说明。 */
      message?: string;
      placeholder?: string;
      /** Primary submit button label (default: common.save). */
      submitLabel?: string;
      onSubmit: (value: string) => void | Promise<void>;
    }
  | null;

const APP_UPDATE_CHECK_INTERVAL_MS = 30 * 60 * 1000;
let appUpdateCheckInFlight: Promise<api.AppUpdateStatus> | null = null;

/** 合并手动与定时检查，避免同一时刻重复请求 GitHub Releases。 */
async function checkForAppUpdate() {
  const request = appUpdateCheckInFlight ??= api.appUpdateCheck();
  try {
    return await request;
  } finally {
    if (appUpdateCheckInFlight === request) {
      appUpdateCheckInFlight = null;
    }
  }
}

export default function App() {
  /** ACP 原生事件归约出的工作区投影（事件监听直接改 ref 内的 view）。 */
  const acpWorkspaceRef = useRef<AcpWorkspaceState>(createAcpWorkspaceState());
  /** 渲染用工作区状态：每次 commit 生成新对象，驱动派生视图与重渲染。 */
  const [acpWorkspace, setAcpWorkspace] = useState<AcpWorkspaceState>(() =>
    createAcpWorkspaceState(),
  );
  /** 当前运行回合的低延迟链路观测；Session 完成后固化进 Assistant 历史。 */
  const turnLatencyBySessionRef = useRef<Map<string, TurnLatencyState>>(
    new Map(),
  );
  /** Host 当前前台请求的稳定关联；值为唯一 requestId。 */
  const activeTurnIdBySessionRef = useRef<Map<string, string>>(new Map());
  /** Host 已完成但 Tauri done 可能仍在跨通道排队的 requestId。 */
  const recoverableCompletedTurnIdBySessionRef = useRef<Map<string, string>>(
    new Map(),
  );
  /** 每个 Session 最近已消费的 requestId，拒绝重放/跨通道迟到更新。 */
  const completedTurnIdBySessionRef = useRef<Map<string, string>>(new Map());
  /** 已完成但当前可见消息尚未 commit 的唯一 DOM 观测。 */
  const pendingVisibleTurnBySessionRef = useRef<Map<string, string>>(
    new Map(),
  );
  /** 从 Host 快照恢复运行中 requestId；null 快照不清理更晚的本地 send。 */
  const observeHostActiveTurn = useCallback(
    (snapshot: {
      sessionId?: string | null;
      activeTurnId?: string | null;
    }) => {
      const sessionId = snapshot.sessionId;
      if (!sessionId) return;
      const localLatency = turnLatencyBySessionRef.current.get(sessionId);
      const resolved = resolveActiveTurnFromHostSnapshot({
        snapshotTurnId: snapshot.activeTurnId,
        localTurnId: localLatency?.turnId,
        completedTurnId: completedTurnIdBySessionRef.current.get(sessionId),
      });
      if (resolved) activeTurnIdBySessionRef.current.set(sessionId, resolved);
      else activeTurnIdBySessionRef.current.delete(sessionId);
      if (
        resolved &&
        recoverableCompletedTurnIdBySessionRef.current.get(sessionId) !==
          resolved
      ) {
        recoverableCompletedTurnIdBySessionRef.current.delete(sessionId);
      }
    },
    [],
  );
  /** 把 ref 中的最新工作区提交到渲染状态。 */
  const commitWorkspace = useCallback(() => {
    setAcpWorkspace({
      sessions: Object.fromEntries(
        Object.entries(acpWorkspaceRef.current.sessions).map(([id, view]) => [
          id,
          { ...view },
        ]),
      ),
    });
  }, []);

  const [themePreference, setThemePreference] = useState<ThemePreference>(() =>
    loadThemePreference(localStorage),
  );
  const [systemTheme, setSystemTheme] = useState<Theme>(() => getSystemTheme());
  const theme = useMemo(
    () => resolveTheme(themePreference, systemTheme),
    [themePreference, systemTheme],
  );
  const [skin, setSkin] = useState<ThemeSkinId>(() => loadSkin(localStorage));
  const [wallpaperRecord, setWallpaperRecord] = useState<WallpaperRecord | null>(
    null,
  );
  const [wallpaperUrl, setWallpaperUrl] = useState<string | null>(null);
  // Holds the current blob: URL so we can revoke it when replacing/clearing.
  const wallpaperUrlRef = useRef<string | null>(null);
  const [wallpaperScrim, setWallpaperScrim] = useState(() =>
    loadWallpaperScrim(localStorage),
  );
  const [layout, setLayout] = useState(() => loadLayout(localStorage));

  const [session, setSession] = useState<SessionSnapshot>(IDLE_SNAPSHOT);
  /** Host live agent (may differ from the session currently viewed in the UI). */
  const [liveHost, setLiveHost] = useState<SessionSnapshot>(IDLE_SNAPSHOT);
  /** 多会话运行状态投影，用于展示后台任务忙碌状态。 */
  const [liveMap, setLiveMap] = useState<SessionLiveMap>({});
  /** 后台正常完成且尚未由用户打开查看的 Session。 */
  const [completedUnreadIds, setCompletedUnreadIds] = useState<Set<string>>(
    () => loadCompletedUnreadSessionIds(localStorage),
  );
  /** Latest live map for callbacks that must not close over a stale render. */
  const liveMapRef = useRef(liveMap);
  liveMapRef.current = liveMap;
  /** Stop interrupt honesty latch (force unlock after budget). */
  const [stopLatch, setStopLatch] = useState<StopLatchState>(() =>
    createStopLatchState(),
  );
  const stopLatchRef = useRef<StopLatchState>(createStopLatchState());
  stopLatchRef.current = stopLatch;
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  /** 当前可见 Session 最近一次由 ACP 上报的上下文用量。 */
  const [contextUsage, setContextUsage] =
    useState<SessionContextUsage | null>(null);
  /** 每个 Session 最近一次由 ACP 上报的真实上下文用量。 */
  const contextUsageBySessionRef = useRef<Map<string, SessionContextUsage>>(
    new Map(),
  );
  /** 从本地请求记录恢复的当前任务整体缓存用量，可跨应用重启。 */
  const [taskCacheUsage, setTaskCacheUsage] =
    useState<api.TaskCacheUsage | null>(null);
  const taskCacheUsageRequestSeqRef = useRef(0);
  /** Composer stored form (may include [[skill:name]] tokens). */
  const [draft, setDraft] = useState("");
  /**
   * 类终端的提示词历史浏览索引（0 表示最新用户消息）。
   * null = not browsing; only engaged when draft empty (or already browsing).
   * Ref tracks live index for key-repeat before React re-renders.
   */
  const [promptHistoryIndex, setPromptHistoryIndex] = useState<number | null>(
    null,
  );
  const promptHistoryIndexRef = useRef<number | null>(null);
  promptHistoryIndexRef.current = promptHistoryIndex;
  /**
   * `/history` + empty-↑ picker — current session prompts only (Build-aligned).
   * Filter focuses on slash open; empty ↑ keeps focus in the composer.
   */
  const [promptHistoryOpen, setPromptHistoryOpen] = useState(false);
  const [promptHistoryFilter, setPromptHistoryFilter] = useState("");
  const [promptHistoryActive, setPromptHistoryActive] = useState(0);
  const [promptHistoryFocusFilter, setPromptHistoryFocusFilter] =
    useState(false);
  const promptHistoryPanelRef = useRef<HTMLDivElement>(null);
  const promptHistoryOpenRef = useRef(false);
  promptHistoryOpenRef.current = promptHistoryOpen;
  /** Prevent overlapping executeSend / queue auto-flush races. */
  const sendInFlightRef = useRef(false);
  const executeSendFromQueueRef = useRef<ExecuteSendFromQueue>(
    async () => false,
  );
  const [skillInfos, setSkillInfos] = useState<SkillInfo[]>([]);
  const [skillsLoading, setSkillsLoading] = useState(false);
  const [slashQuery, setSlashQuery] = useState<{
    start: number;
    query: string;
    end: number;
  } | null>(null);
  /**
   * Live slash token from contenteditable.innerText (rAF poll).
   * Independent of React draft so IME / <br> / missed onChange cannot desync.
   * `present` is true for bare `/` as well as `/query`.
   */
  const [liveSlash, setLiveSlash] = useState<{
    present: boolean;
    query: string;
    start: number;
    end: number;
  }>({ present: false, query: "", start: 0, end: 0 });
  const liveSlashRef = useRef(liveSlash);
  liveSlashRef.current = liveSlash;
  /** After Escape, suppress re-open until the `/token` text changes. */
  const slashDismissedSigRef = useRef<string | null>(null);
  const showComposerPlusRef = useRef(false);
  const [slashActiveIndex, setSlashActiveIndex] = useState(0);
  const [showStatusModal, setShowStatusModal] = useState(false);
  const [projects, setProjects] = useState<Project[]>([]);
  const [addProjectIntent, setAddProjectIntent] = useState<{
    bindSession: boolean;
  } | null>(null);
  const [addProjectName, setAddProjectName] = useState("");
  const [addProjectPath, setAddProjectPath] = useState("");
  const [addProjectBusy, setAddProjectBusy] = useState(false);
  const [addProjectError, setAddProjectError] = useState<string | null>(null);
  const addProjectNameRef = useRef<HTMLInputElement>(null);
  const addProjectDropRef = useRef<HTMLButtonElement>(null);
  const addProjectReturnFocusRef = useRef<HTMLElement | null>(null);
  const addProjectSourceRequestRef = useRef(0);
  const addProjectNameEditedRef = useRef(false);
  const [sessions, setSessions] = useState<SessionRow[]>([]);
  /** 自动标题异步持久化时读取最新任务列表，避免覆盖用户手动重命名。 */
  const sessionsRef = useRef<SessionRow[]>([]);
  sessionsRef.current = sessions;
  /** 当前进程内已确认的标题覆盖值，防止异步列表刷新覆盖新标题。 */
  const sessionTitleOverridesRef = useRef<Map<string, string>>(new Map());
  /** 同一 Session 同时只允许一条自动标题持久化任务。 */
  const autoTitleInFlightRef = useRef<Set<string>>(new Set());
  /** 同一 Session 在当前运行期只尝试一次 Agent 标题请求。 */
  const autoTitleAttemptedRef = useRef<Set<string>>(new Set());
  const [activeProject, setActiveProject] = useState<Project | null>(null);
  /** Per-session message cache so switching away mid-turn does not drop the UI. */
  const messagesBySessionRef = useRef<Map<string, ChatMessage[]>>(new Map());
  /** 每个会话最后确认的模型，避免切换对话时复用全局 composer 模型。 */
  const modelBySessionRef = useRef<Map<string, string>>(new Map());
  const viewingSessionIdRef = useRef<string | null>(null);
  const [subagentDescriptions, setSubagentDescriptions] = useState<
    Record<string, string>
  >({});
  /** 当前渲染 Session 的 ACP 原生视图；草稿没有持久化视图。 */
  const acpSessionView = useMemo(
    () =>
      session.sessionId
        ? acpWorkspace.sessions[session.sessionId] ?? null
        : null,
    [acpWorkspace, session.sessionId],
  );
  const displayedSubagents = useMemo(
    () =>
      withSubagentPrompts(messages, acpSessionView?.subagents ?? []).map(
        (agent) => ({
          ...agent,
          agent_description: subagentDescriptions[agent.agent_name],
        }),
      ),
    [acpSessionView?.subagents, messages, subagentDescriptions],
  );
  /**
   * Bumped on every user navigation (open chat / new chat). Async work captures
   * {@link currentViewFocus} before its first await and must re-check before
   * touching view state — otherwise a slow connect started on one draft yanks
   * the workbench away from the draft the user opened since.
   */
  const viewEpochRef = useRef(0);
  const currentViewFocus = useCallback(
    (): ViewFocus => ({
      sessionId: viewingSessionIdRef.current,
      epoch: viewEpochRef.current,
    }),
    [],
  );
  const bumpViewEpoch = useCallback(() => {
    viewEpochRef.current += 1;
  }, []);
  const liveHostRef = useRef<SessionSnapshot>(IDLE_SNAPSHOT);
  const messagesRef = useRef<ChatMessage[]>([]);
  const [expandedProjects, setExpandedProjects] = useState<Record<string, boolean>>({});
  const [visibleSessionsByProject, setVisibleSessionsByProject] = useState<
    Record<string, number>
  >({});
  const [sessionOrder, setSessionOrder] = useState(() => loadSessionOrder());
  const draggedSidebarItemRef = useRef<{ kind: "project" | "session"; id: string } | null>(null);
  const [projectDropHint, setProjectDropHint] = useState<{
    id: string;
    after: boolean;
  } | null>(null);
  const projectReorderRevisionRef = useRef(0);
  const projectReorderQueueRef = useRef<Promise<void>>(Promise.resolve());
  /** Avoid writing collapse prefs before settings hydrate on launch. */
  const expandedProjectsHydratedRef = useRef(false);
  const [projectsOpen, setProjectsOpen] = useState(true);
  /** 置顶任务栏目是否展开。 */
  const [pinnedOpen, setPinnedOpen] = useState(true);
  const [historyOpen, setHistoryOpen] = useState(true);
  const [ctxMenu, setCtxMenu] = useState<ContextMenuState>(null);
  const [appDialog, setAppDialog] = useState<AppDialog>(null);
  const [dialogInput, setDialogInput] = useState("");
  const dialogInputRef = useRef<HTMLInputElement>(null);
  const confirmBtnRef = useRef<HTMLButtonElement>(null);
  /** Latest dialog for Enter/Escape handlers (avoids stale chained confirms). */
  const appDialogRef = useRef<AppDialog>(null);
  appDialogRef.current = appDialog;
  const [showSearch, setShowSearch] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const searchTriggerRef = useRef<HTMLButtonElement>(null);
  const searchReturnFocusRef = useRef<HTMLElement | null>(null);
  const [showComposerPlus, setShowComposerPlus] = useState(false);
  const [composerPanel, setComposerPanel] = useState<
    "model" | "reasoning" | null
  >(null);
  showComposerPlusRef.current = showComposerPlus;
  const composerPlusTriggerRef = useRef<HTMLButtonElement>(null);
  const composerPlusPanelRef = useRef<HTMLDivElement>(null);
  const composerInputRef = useRef<HTMLDivElement>(null);
  const openSearch = useCallback(() => {
    const active = document.activeElement;
    const activeSidebarWidth =
      active instanceof HTMLElement
        ? active.closest(".sidebar")?.getBoundingClientRect().width
        : null;
    const searchTrigger = searchTriggerRef.current;
    const sidebarWidth = searchTrigger
      ?.closest(".sidebar")
      ?.getBoundingClientRect().width;
    searchReturnFocusRef.current =
      active instanceof HTMLElement &&
      active !== document.body &&
      activeSidebarWidth !== 0
        ? active
        : sidebarWidth
          ? searchTrigger
          : composerInputRef.current;
    setSearchQuery("");
    setShowSearch(true);
  }, []);
  /** Actual input card (.composer) — command panel anchors here. */
  const composerShellRef = useRef<HTMLDivElement>(null);
  /** Floating composer shell — height drives chat bottom padding. */
  const composerWrapRef = useRef<HTMLDivElement>(null);
  const askUserWrapRef = useRef<HTMLDivElement>(null);
  const [composerFloatPad, setComposerFloatPad] = useState(168);
  /** Set by newChat; applied after chat pane + textarea mount. */
  const pendingComposerFocus = useRef(false);
  /** Desktop Connect panel (AC7) — close does not stop host. */

  /** 哈希路由仅允许工作台与设置分区。 */
  const [appView, setAppView] = useState<"workbench" | "settings">("workbench");
  const [settingsSection, setSettingsSection] =
    useState<SettingsSectionId>("general");
  /** While openSession loads, do not let session.sessionId effect clobber viewing id. */
  const openingSessionIdRef = useRef<string | null>(null);
  /** Distinguishes two overlapping opens of the same Session. */
  const openingSessionEpochRef = useRef<number | null>(null);

  // ContextMenu handles outside click + Escape for sidebar menus.

  useEffect(() => {
    if (!appDialog) return;
    if (appDialog.kind === "prompt") {
      setDialogInput(appDialog.initial);
      const t = window.setTimeout(() => {
        dialogInputRef.current?.focus();
        dialogInputRef.current?.select();
      }, 0);
      return () => window.clearTimeout(t);
    }
    // Confirm: focus primary action so keyboard users land on Confirm.
    // Enter is also handled globally below so it still confirms if focus
    // sits on Cancel / close (needed for reliable multi-step confirmation).
    if (appDialog.kind === "confirm") {
      const t = window.setTimeout(() => {
        confirmBtnRef.current?.focus();
      }, 0);
      return () => window.clearTimeout(t);
    }
  }, [appDialog]);

  useEffect(() => {
    if (!api.isTauri()) return;
    let disposed = false;
    let unlistenClose: (() => void) | undefined;
    let unlistenExit: (() => void) | undefined;

    const showExitConfirmation = (activeCount: number) => {
      if (disposed) return;
      setAppDialog({
        kind: "confirm",
        title: "退出 KeenCode？",
        message: `仍有 ${activeCount} 个任务正在运行。退出会中断这些任务及其启动的终端进程。下次启动后，你可以进入原任务并手动输入“继续”。`,
        confirmLabel: "停止任务并退出",
        danger: true,
        onConfirm: api.appConfirmExit,
      });
    };

    void (async () => {
      const [{ getCurrentWindow }, { listen }] = await Promise.all([
        import("@tauri-apps/api/window"),
        import("@tauri-apps/api/event"),
      ]);
      unlistenExit = await listen<{ activeCount: number }>(
        "app://exit-requested",
        (event) => showExitConfirmation(event.payload.activeCount),
      );
      unlistenClose = await getCurrentWindow().onCloseRequested((event) => {
        event.preventDefault();
        void api.appRequestExit();
      });
      if (disposed) {
        unlistenExit();
        unlistenClose();
      }
    })();

    return () => {
      disposed = true;
      unlistenExit?.();
      unlistenClose?.();
    };
  }, []);

  useEffect(() => {
    if (!appDialog) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setAppDialog(null);
        return;
      }
      // Confirm dialogs: Enter always accepts, including chained confirmations.
      // Capture phase + preventDefault so we don't double-fire with a focused
      // submit button's native activation.
      if (e.key !== "Enter" && e.key !== "NumpadEnter") return;
      if (e.isComposing || e.altKey || e.ctrlKey || e.metaKey) return;
      const dialog = appDialogRef.current;
      if (!dialog || dialog.kind !== "confirm") return;
      e.preventDefault();
      e.stopPropagation();
      const run = dialog.onConfirm;
      setAppDialog(null);
      void run();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [appDialog]);

  useEffect(() => {
    if (!showSearch) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setShowSearch(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [showSearch]);

  // 全局快捷键：搜索、任务内查找、帮助、新建任务和设置。
  // Handlers go through refs so we don't re-bind every render.
  const shortcutHandlersRef = useRef({
    newChat: () => {},
    openSettings: () => {},
    openChatFind: () => {},
  });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.isComposing) return;
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName?.toLowerCase();
      const typing =
        tag === "input" ||
        tag === "textarea" ||
        !!target?.isContentEditable;
      const key = e.key.toLowerCase();
      // In-chat find — open even while typing in the composer.
      if (key === "f" && !e.shiftKey) {
        e.preventDefault();
        shortcutHandlersRef.current.openChatFind();
        return;
      }
      if (key === "k") {
        e.preventDefault();
        openSearch();
        return;
      }
      if (key === "/") {
        e.preventDefault();
        setShowShortcuts((v) => !v);
        return;
      }
      if (key === "," && !typing) {
        e.preventDefault();
        shortcutHandlersRef.current.openSettings();
        return;
      }
      if (key === "n" && !typing) {
        e.preventDefault();
        shortcutHandlersRef.current.newChat();
        return;
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [openSearch]);

  /** 首次渲染时展示品牌启动页；工作台外壳不等待会话状态。 */
  const [appBooting, setAppBooting] = useState(true);
  const [chromeHardwareAcceleration, setChromeHardwareAcceleration] =
    useState(true);
  const [customInstructions, setCustomInstructions] = useState("");
  const [memoryFile, setMemoryFile] = useState("");
  const [localMemories, setLocalMemories] = useState(true);
  const [taskNotifications, setTaskNotifications] = useState(true);
  const [notificationSound, setNotificationSound] = useState(true);
  const [autoArchiveConversations, setAutoArchiveConversations] = useState<boolean | null>(null);
  const [archiveRetentionDays, setArchiveRetentionDays] = useState(7);
  const [archiveClock, setArchiveClock] = useState(0);
  const [goalModeSessionKey, setGoalModeSessionKey] = useState<string | null>(
    null,
  );
  /** 计划模式激活的会话键（`sessionId ?? "__draft__"`）；null = 未激活。 */
  const [planModeSessionKey, setPlanModeSessionKey] = useState<string | null>(
    null,
  );
  /** Ultra 委派策略激活的会话键；与 Goal、Plan 和推理强度相互独立。 */
  const [ultraModeSessionKey, setUltraModeSessionKey] = useState<string | null>(
    null,
  );
  const [appUpdateDownloadSource, setAppUpdateDownloadSource] =
    useState<api.AppUpdateDownloadSource>("auto");
  const [keepComputerAwake, setKeepComputerAwake] = useState(true);
  const [backgroundAgentLimit, setBackgroundAgentLimit] = useState(10);
  const [projectDirectory, setProjectDirectory] = useState("");
  const [locale, setLocale] = useState<Locale>("zh");

  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  // 从本地设置文件恢复常规选项。
  useEffect(() => {
    if (appBooting || !api.isTauri()) return;
    void api
      .settingsGet()
      .then((settings) => {
        setChromeHardwareAcceleration(settings.chromeHardwareAcceleration);
        setTaskNotifications(settings.taskNotifications);
        setNotificationSound(settings.notificationSound);
        setAppUpdateDownloadSource(settings.appUpdateDownloadSource);
        setKeepComputerAwake(settings.keepComputerAwake);
        setBackgroundAgentLimit(settings.backgroundAgentLimit);
        setProjectDirectory(settings.projectDirectory);
        setLocalMemories(settings.localMemories);
        setAutoArchiveConversations(settings.autoArchiveConversations);
        setArchiveRetentionDays(settings.archiveRetentionDays);
        setLocale(settings.interfaceLanguage);
      })
      .catch(() => {});
    void api
      .customInstructionsGet()
      .then(setCustomInstructions)
      .catch(() => {});
    void api
      .memoriesGet()
      .then(setMemoryFile)
      .catch(() => {});
  }, [appBooting]);

  useEffect(() => {
    if (!autoArchiveConversations) return;
    const now = Date.now();
    const preferences = autoArchiveExpiredSessions(sessions, archiveRetentionDays, now);
    setSessions((current) => {
      const next = current.map((item) => ({
        ...item,
        archived: preferences[item.id]?.archived ?? item.archived,
      }));
      return next.some((item, index) => item.archived !== current[index]?.archived)
        ? next
        : current;
    });
    const nextExpiry = sessions.reduce((next, item) => {
      const preference = preferences[item.id];
      if (preference?.pinned || preference?.archived) return next;
      const expiry = Date.parse(item.updatedAt) + archiveRetentionDays * 86_400_000;
      return Number.isFinite(expiry) && expiry > now ? Math.min(next, expiry) : next;
    }, Number.POSITIVE_INFINITY);
    if (!Number.isFinite(nextExpiry)) return;
    const timer = window.setTimeout(
      () => setArchiveClock((value) => value + 1),
      Math.min(nextExpiry - now, 2_147_483_647),
    );
    return () => window.clearTimeout(timer);
  }, [archiveClock, archiveRetentionDays, autoArchiveConversations, sessions]);
  const [showShortcuts, setShowShortcuts] = useState(false);
  /** In-conversation find (Cmd/Ctrl+F) — not the palette/session search. */
  const [showChatFind, setShowChatFind] = useState(false);
  const [chatFindQuery, setChatFindQuery] = useState("");
  const [chatFindIndex, setChatFindIndex] = useState(0);
  const [askUser, setAskUser] = useState<AskUserPayload | null>(null);
  /**
   * Unanswered gates per session (`sessionId` → payload).
   *
   * 后台任务也可以在用户查看其他任务时提出问题。这里按 Session 暂存未回答问题，
   * 切回任务时恢复显示，回答或本轮结束后删除。
   */
  const pendingAskUserBySessionRef = useRef<Map<string, AskUserPayload>>(
    new Map(),
  );
  /** 触发侧栏重渲染，使后台等待输入的任务显示状态点。 */
  const [pendingAskUserSessionIds, setPendingAskUserSessionIds] = useState<Set<string>>(
    new Set(),
  );
  /** 清除指定 Session 尚未回答的问题；请求标识不同时保留后来到达的新问题。 */
  const clearPendingAskUser = useCallback(
    (sessionId?: string | null, rpcId?: number) => {
      if (!sessionId) return;
      const pending = pendingAskUserBySessionRef.current.get(sessionId);
      if (rpcId != null && pending?.rpcId !== rpcId) return;
      pendingAskUserBySessionRef.current.delete(sessionId);
      setPendingAskUserSessionIds((previous) => {
        if (!previous.has(sessionId)) return previous;
        const next = new Set(previous);
        next.delete(sessionId);
        return next;
      });
    },
    [],
  );
  /** 为只挂载一次的事件监听保存最新问题清理函数。 */
  const clearPendingAskUserRef = useRef(clearPendingAskUser);
  clearPendingAskUserRef.current = clearPendingAskUser;
  /** Polite SR announce for stream start/stop (not every token). */
  const [streamA11yNote, setStreamA11yNote] = useState("");
  const wasStreamingRef = useRef(false);
  const localeRef = useRef(locale);
  localeRef.current = locale;
  const tr = useMemo(() => createT(locale), [locale]);
  const trRef = useRef(tr);
  trRef.current = tr;
  const [appUpdateStatus, setAppUpdateStatus] =
    useState<api.AppUpdateStatus | null>(null);
  const [appUpdateBusy, setAppUpdateBusy] = useState<
    "checking" | "installing" | null
  >(null);
  const [appUpdateError, setAppUpdateError] = useState<string | null>(null);
  const [appUpdateProgressOpen, setAppUpdateProgressOpen] = useState(false);

  // 后端下载任务通过事件推送进度；仅在任务活动时产生事件，不增加空闲轮询。
  useEffect(() => {
    if (!api.isTauri()) return;
    let active = true;
    let unlisten: (() => void) | undefined;
    void api
      .listen<api.AppUpdateStatus>(
        api.APP_UPDATE_STATUS_EVENT,
        (status) => {
          if (!active) return;
          setAppUpdateStatus(status);
          if (status.downloadState !== "failed") {
            setAppUpdateError(null);
          }
        },
      )
      .then((stop) => {
        if (active) unlisten = stop;
        else stop();
      })
      .catch(() => {});
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  const installAppUpdate = useCallback(async () => {
    if (!api.isTauri()) return;
    setAppUpdateProgressOpen(true);
    setAppUpdateBusy("installing");
    setAppUpdateError(null);
    try {
      await api.appUpdateInstall();
    } catch (error) {
      setAppUpdateError(localizeUiError(error, locale));
      setAppUpdateProgressOpen(true);
      void api
        .appUpdateInfo()
        .then(setAppUpdateStatus)
        .catch(() => {});
    } finally {
      setAppUpdateBusy(null);
    }
  }, []);

  const checkAppUpdate = useCallback(async () => {
    if (!api.isTauri()) return;
    setAppUpdateBusy("checking");
    setAppUpdateError(null);
    try {
      setAppUpdateStatus(await checkForAppUpdate());
    } catch (error) {
      setAppUpdateError(localizeUiError(error, locale));
    } finally {
      setAppUpdateBusy(null);
    }
  }, []);

  // 启动后立即静默检查，并每 30 分钟复查；失败时保持安静，下一轮自动重试。
  useEffect(() => {
    if (appBooting || !api.isTauri()) return;
    let active = true;
    void api
      .appUpdateInfo()
      .then((status) => {
        if (active) {
          setAppUpdateStatus((current) => current ?? status);
        }
      })
      .catch(() => {});
    const check = () => {
      void checkForAppUpdate()
        .then((status) => {
          if (active) setAppUpdateStatus(status);
        })
        .catch(() => {});
    };
    check();
    const timer = window.setInterval(check, APP_UPDATE_CHECK_INTERVAL_MS);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [appBooting]);

  const [modelId, setModelId] = useState("");
  const [effort, setEffort] = useState(DEFAULT_EFFORT);
  /** 当前自定义供应商配置的原始模型列表。 */
  const [configuredModels, setConfiguredModels] = useState<ModelOption[]>([]);
  /** 事件监听回调注册时闭包固定，需要最新模型目录时走 ref 镜像。 */
  const configuredModelsRef = useRef(configuredModels);
  configuredModelsRef.current = configuredModels;
  /** 已按 modelId 查询并缓存到当前界面的模型元数据。 */
  const [modelMetadataById, setModelMetadataById] = useState<
    Record<string, api.ModelMetadata>
  >({});
  /** 将按需元数据投影到模型菜单，供应商仅用于路由，不参与元数据查询。 */
  const availableModels = useMemo(
    () =>
      configuredModels.map((model) => {
        const merged = applyModelMetadata(model, modelMetadataById[model.id]);
        // 供应商手工配置的上下文窗口优先于元数据目录值。
        if (model.contextWindow) {
          return { ...merged, contextWindow: model.contextWindow };
        }
        return merged;
      }),
    [configuredModels, modelMetadataById],
  );
  const modelLabel =
    availableModels.find((model) => model.id === modelId)?.label ?? modelId;
  const [subagentModelLabels, setSubagentModelLabels] = useState<
    Record<string, string>
  >({});
  const subagentIdentityKey = displayedSubagents
    .map((agent) => `${agent.agent_id}:${agent.agent_name}`)
    .join("|");
  useEffect(() => {
    if (!api.isTauri() || !subagentIdentityKey) return;
    let cancelled = false;
    void api
      .agentsList()
      .then(({ agents }) => {
        if (cancelled) return;
        setSubagentDescriptions(
          Object.fromEntries(
            agents.map((agent) => [agent.name, agent.description.trim()]),
          ),
        );
        setSubagentModelLabels(
          Object.fromEntries(
            agents.flatMap((agent) => {
              if (!agent.model) return [];
              const model = agent.model.split("::").at(-1)?.trim();
              return model ? [[agent.name, model]] : [];
            }),
          ),
        );
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [subagentIdentityKey]);
  /** Files/folders attached for next send (@path to agent). */
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const claimedClipboardFilesRef = useRef(new Set<string>());
  /** Chat file/url card → open in right resource pane. */
  const [resourceOpenTarget, setResourceOpenTarget] =
    useState<ResourceOpenTarget | null>(null);
  /** 对话右上角的环境与子 Agent 摘要浮层。 */
  const [summaryOpen, setSummaryOpen] = useState(false);
  const previousAsideCollapsedRef = useRef(layout.asideCollapsed);
  useLayoutEffect(() => {
    const previous = previousAsideCollapsedRef.current;
    previousAsideCollapsedRef.current = layout.asideCollapsed;
    if (previous === layout.asideCollapsed) return;
    setSummaryOpen(layout.asideCollapsed && Boolean(session.sessionId));
  }, [layout.asideCollapsed, session.sessionId]);
  /** 任务摘要按钮引用，供浮层判断点击是否来自触发按钮。 */
  const summaryTriggerRef = useRef<HTMLButtonElement>(null);
  /** 关闭任务摘要浮层，避免流式更新期间反复重绑文档监听。 */
  const closeSummary = useCallback(() => setSummaryOpen(false), []);
  /** Agent 工具状态变化时驱动右侧文件树与 Git 状态同步。 */
  const resourceSyncRevision = useMemo(
    () =>
      messages.reduce((revision, message) => {
        for (const segment of message.segments ?? []) {
          if (segment.kind !== "tool") continue;
          for (const char of `${segment.toolCallId}:${segment.status}:${segment.streaming ?? false}`) {
            revision = (revision * 31 + char.charCodeAt(0)) >>> 0;
          }
        }
        return revision;
      }, 0),
    [messages],
  );
  /** Live drag-drop target for zone overlays (null = not dragging). */
  const [dragZone, setDragZone] = useState<DragZone>(null);
  const [toast, setToast] = useState<string | null>(null);
  const dragPathsRef = useRef<string[]>([]);
  const [localError, setLocalError] = useState<string | null>(null);
  /** Expand technical dump under the compact error banner. */
  const [errorDetailOpen, setErrorDetailOpen] = useState(false);
  const sendRef = useRef<(() => Promise<void>) | null>(null);
  const [gitWorktrees, setGitWorktrees] = useState<api.GitWorktreeEntry[]>([]);
  /** null = unknown/loading; true = git work tree; false = not a git repo. */
  const [gitWorktreesAvailable, setGitWorktreesAvailable] = useState<
    boolean | null
  >(null);
  const [gitWorktreesLoading, setGitWorktreesLoading] = useState(false);
  const [gitWorktreesReason, setGitWorktreesReason] = useState<string | null>(
    null,
  );
  /** New worktree dialog (name + optional start-point). */
  const [worktreeCreateOpen, setWorktreeCreateOpen] = useState(false);
  const [worktreeCreateName, setWorktreeCreateName] = useState("");
  const [worktreeCreateRef, setWorktreeCreateRef] = useState("");
  const [worktreeCreateBusy, setWorktreeCreateBusy] = useState(false);
  const [worktreeCreateError, setWorktreeCreateError] = useState<string | null>(
    null,
  );
  /** When true, after create bind cwd and open a draft chat on that path. */
  const [worktreeCreateStartChat, setWorktreeCreateStartChat] = useState(false);
  /** Clean stale worktrees (git worktree prune) dialog. */
  const [worktreeGcOpen, setWorktreeGcOpen] = useState(false);
  const [worktreeGcForce, setWorktreeGcForce] = useState(false);
  const [worktreeGcBusy, setWorktreeGcBusy] = useState(false);
  const [worktreeGcPreviewBusy, setWorktreeGcPreviewBusy] = useState(false);
  const [worktreeGcError, setWorktreeGcError] = useState<string | null>(null);
  const [worktreeGcPreview, setWorktreeGcPreview] =
    useState<api.GitWorktreeGcResult | null>(null);
  /** Host stream-stall prompt (I06); null when dismissed or not stalled. */
  const [streamStall, setStreamStall] = useState<{
    sessionId?: string;
    stallSeconds: number;
    tier?: string;
    sawModelOutput?: boolean;
    sawToolActivity?: boolean;
  } | null>(null);
  const [connecting, setConnecting] = useState(false);
  /** Sync gate for ensureConnected (React state alone races two rapid sends). */
  const connectingRef = useRef(false);
  /** Live provider retry progress (session://retry); cleared on success/stop/error. */
  const [retryStatus, setRetryStatus] = useState<{
    attempt: number;
    maxAttempts: number;
    reason: string;
  } | null>(null);
  /** Epoch ms when the current agent turn became busy (for elapsed UI). */
  const [turnStartedAt, setTurnStartedAt] = useState<number | null>(null);
  const [resizingAside, setResizingAside] = useState(false);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const platform = useMemo(() => {
    const ua = navigator.userAgent.toLowerCase();
    if (ua.includes("mac")) return "mac" as const;
    if (ua.includes("win")) return "win" as const;
    return "other" as const;
  }, []);
  /** Self-drawn chrome when OS title bar is disabled (Windows release config). */
  const useCustomWindowChrome = platform === "win" || platform === "other";
  const [windowMaximized, setWindowMaximized] = useState(false);
  /** macOS 全屏时红绿灯收入顶部悬停区，标题栏安全边距应收回。 */
  const [windowFullscreen, setWindowFullscreen] = useState(false);

  // Keep data-theme + native chrome in sync with the resolved theme.
  // When preference is "system", native must stay unlocked (null) so the
  // WebView continues to receive OS scheme changes via matchMedia.
  useEffect(() => {
    applyThemeToDocument(theme);
    void applyNativeWindowTheme(
      themePreference === "system" ? null : theme,
    );
  }, [theme, themePreference]);

  // Follow OS light/dark: re-read immediately on enter, then live-subscribe.
  useEffect(() => {
    if (themePreference !== "system") return;
    let cancelled = false;
    void (async () => {
      // Unlock native first so getSystemTheme() sees the real OS scheme.
      await applyNativeWindowTheme(null);
      if (cancelled) return;
      const sys = getSystemTheme();
      setSystemTheme(sys);
      applyThemeToDocument(sys);
    })();
    const unsub = subscribeSystemTheme((next) => {
      setSystemTheme(next);
      applyThemeToDocument(next);
      // Keep native unlocked while following system.
      void applyNativeWindowTheme(null);
    });
    return () => {
      cancelled = true;
      unsub();
    };
  }, [themePreference]);

  useEffect(() => {
    applySkinToDocument(skin);
  }, [skin]);

  // 从 IndexedDB 唯一事实源加载完整壁纸记录并创建媒体对象 URL。
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const rec = await loadWallpaperRecord();
        if (cancelled || !rec) return;
        const url = URL.createObjectURL(rec.blob);
        wallpaperUrlRef.current = url;
        setWallpaperRecord(rec);
        setWallpaperUrl(url);
      } catch (error) {
        if (!cancelled) setLocalError(localizeUiError(error, locale));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Keep the data-wallpaper flag in sync when the user uploads / clears.
  useEffect(() => {
    applyWallpaperFlag(wallpaperUrl !== null);
  }, [wallpaperUrl]);

  // Scrim strength only dims the wallpaper overlay (::after), not chrome.
  useEffect(() => {
    applyWallpaperScrimToDocument(wallpaperScrim);
  }, [wallpaperScrim]);

  useEffect(() => {
    document.documentElement.classList.remove(
      "platform-mac",
      "platform-win",
      "platform-other",
    );
    if (platform === "mac") document.documentElement.classList.add("platform-mac");
    if (platform === "win") document.documentElement.classList.add("platform-win");
    if (platform === "other") document.documentElement.classList.add("platform-other");
  }, [platform]);

  useEffect(() => {
    if (!api.isTauri()) return;
    let unlistenResize: (() => void) | undefined;
    let cancelled = false;
    void (async () => {
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const w = getCurrentWindow();
        const sync = async () => {
          try {
            const [maximized, fullscreen] = await Promise.all([
              w.isMaximized(),
              w.isFullscreen(),
            ]);
            if (useCustomWindowChrome) setWindowMaximized(maximized);
            setWindowFullscreen(fullscreen);
          } catch {
            /* ignore */
          }
        };
        await sync();
        unlistenResize = await w.onResized(() => {
          void sync();
        });
        if (cancelled) unlistenResize?.();
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
      unlistenResize?.();
    };
  }, [useCustomWindowChrome]);

  const refreshLists = useCallback(async () => {
    // Tauri setup 已在 WebView 挂载前完成后端初始化。当前 Session 的
    // connecting/streaming/disconnected 是会话状态，不是应用启动门禁。
    // 先开放工作台，再异步恢复本地项目与会话，任何状态都不能困住用户。
    setAppBooting(false);
    if (!api.isTauri()) {
      // 浏览器只用于保留上游界面的静态开发预览。
      return;
    }
    const phase = "sessions_list/projects_list";
    try {
      const [rows, persistedProjects] = await Promise.all([
        sessionsList(),
        api.projectsList() as Promise<Project[]>,
      ]);
      const projection = projectSidebar(
        rows,
        loadSessionPreferences(),
        persistedProjects,
      );
      setProjects(projection.projects);
      setSessions(projection.sessions);
      setActiveProject((prev) => {
        if (prev && projection.projects.some((project) => project.id === prev.id)) {
          return projection.projects.find((project) => project.id === prev.id) || prev;
        }
        return null;
      });
      setExpandedProjects(Object.fromEntries(
        projection.projects.map((project) => [project.id, false]),
      ));
      setLocalError(null);
      expandedProjectsHydratedRef.current = true;
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      await diagnosticsRecord("frontend.refresh_lists", `${phase}: ${message}`).catch(() => {});
      console.error("[keencode] initial workspace data load failed", {
        phase,
        cause,
      });
      setLocalError("KeenCode 无法加载本地工作区数据，请稍后重试。");
    }
  }, []);

  // Bootstrap lists once
  useEffect(() => {
    void refreshLists();
  }, [refreshLists]);

  /** 从持久化请求事实刷新任务整体缓存率；序号隔离快速切换任务的迟到结果。 */
  const refreshTaskCacheUsage = useCallback(async (sessionId: string | null) => {
    const requestSeq = ++taskCacheUsageRequestSeqRef.current;
    if (!sessionId || !api.isTauri()) {
      setTaskCacheUsage(null);
      return;
    }
    try {
      const usage = await api.taskCacheUsageGet(sessionId);
      if (
        requestSeq === taskCacheUsageRequestSeqRef.current &&
        viewingSessionIdRef.current === sessionId
      ) {
        setTaskCacheUsage(usage);
      }
    } catch (error) {
      if (
        requestSeq === taskCacheUsageRequestSeqRef.current &&
        viewingSessionIdRef.current === sessionId
      ) {
        setTaskCacheUsage(null);
      }
      console.warn("load task cache usage failed", error);
    }
  }, []);

  useEffect(() => {
    void refreshTaskCacheUsage(session.sessionId);
  }, [refreshTaskCacheUsage, session.sessionId]);

  /** 将 acpWorkspace 中指定 Session 的最新投影应用到工作台组件。 */
  const applyViewProjection = useCallback((session_id: string | null) => {
    if (!session_id) return;
    const view = acpWorkspaceRef.current.sessions[session_id];
    if (!view) return;
    const projectedSnapshot = projectAcpSnapshot(view);
    const preferredTitle =
      sessionTitleOverridesRef.current.get(session_id) ??
      sessionsRef.current.find((row) => row.id === session_id)?.title;
    const snapshot = preferredTitle
      ? { ...projectedSnapshot, title: preferredTitle }
      : projectedSnapshot;
    const reportedUsage = contextUsageBySessionRef.current.get(session_id);
    setContextUsage(reportedUsage ?? null);
    setSession(snapshot);
    setRetryStatus(
      view.retry
          ? {
              attempt: view.retry.attempt,
              maxAttempts: view.retry.maxAttempts,
              reason: view.retry.reason,
            }
        : null,
    );
    if (view.reasoning_effort) setEffort(view.reasoning_effort);
    setLiveHost(snapshot);
    liveHostRef.current = snapshot;
    setLiveMap((previous) =>
      projectHostIntoLiveMap(previous, {
        sessionId: session_id,
        state: snapshot.state,
        streamingMessageId: snapshot.streamingMessageId,
      }),
    );
    setMessages((previous) => {
      const hasLocalPendingAssistant = previous.some(
        (message) =>
          message.role === "assistant" &&
          message.id.startsWith("a-pending-") &&
          message.streaming === true,
      );
      const keepPendingAssistant =
        view.status === "streaming" ||
        activeTurnIdBySessionRef.current.has(session_id) ||
        (sendInFlightRef.current && hasLocalPendingAssistant);
      const next = projectAcpConversation(
        previous,
        view,
        locale,
        keepPendingAssistant,
      );
      messagesBySessionRef.current.set(session_id, next);
      return next;
    });

  }, [locale]);
  /** 事件监听与异步流程用最新 applyViewProjection。 */
  const applyViewProjectionRef = useRef(applyViewProjection);
  applyViewProjectionRef.current = applyViewProjection;

  /** 订阅 ACP 事件：归约到工作区并投影当前查看的 Session。 */
  useEffect(() => {
    if (!api.isTauri()) return;
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const pendingProjectionSessions = new Set<string>();
    const publishScheduledEvents = () => {
      if (disposed) return;
      const viewingSessionId = viewingSessionIdRef.current;
      const shouldProjectViewing =
        viewingSessionId != null &&
        pendingProjectionSessions.has(viewingSessionId);
      pendingProjectionSessions.clear();
      commitWorkspace();
      if (shouldProjectViewing) {
        applyViewProjectionRef.current(viewingSessionId);
      }
    };
    const projectionBatcher = createAnimationFrameBatcher(
      publishScheduledEvents,
      (callback) => requestAnimationFrame(callback),
      (id) => cancelAnimationFrame(id),
    );
    const scheduleProjection = (sessionId: string) => {
      pendingProjectionSessions.add(sessionId);
      projectionBatcher.schedule();
    };
    const flushProjection = (sessionId: string) => {
      pendingProjectionSessions.add(sessionId);
      if (viewingSessionIdRef.current === sessionId) {
        projectionBatcher.flush();
        return;
      }
      // 后台 Session 的边界并入下一绘制帧，不能借机提前发布当前会话
      // 尚在等待绘制帧的 text/thought；liveMap 已单独同步关键忙闲状态。
      projectionBatcher.schedule();
    };
    const activeTurnsBeforeBootstrap = new Map(
      activeTurnIdBySessionRef.current,
    );
    const correlatedTurnId = (sessionId: string) =>
      activeTurnIdBySessionRef.current.get(sessionId) ??
      recoverableCompletedTurnIdBySessionRef.current.get(sessionId);
    const activeTurnBootstrap =
      createActiveTurnBootstrapBuffer(correlatedTurnId);
    const registrationPromises: Array<Promise<() => void>> = [];
    void (async () => {
      registrationPromises.push(
        listenAcp("acp://session-update", (notification) => {
          if (disposed) return;
          const params = notification.params;
          if (!params?.sessionId) return;
          const view = ensureAcpSession(
            acpWorkspaceRef.current,
            params.sessionId,
          );
          const sourceAgentId = resolveSessionUpdateSourceAgentId(
            view,
            params._peri?.sourceAgentId,
          );
          const apply = () => {
          const activeRequestId = correlatedTurnId(params.sessionId);
          if (
            !shouldApplySessionUpdate(
              params,
              activeRequestId,
              sourceAgentId,
            )
          ) {
            return;
          }
          const tag = params.update.sessionUpdate;
          const hadVisibleMainText = view.live_segments.some(
            (segment) =>
              (segment.kind === "thought" || segment.kind === "content") &&
              segment.text.trim().length > 0,
          );
          if (
            tag === "usage_update" &&
            !sourceAgentId &&
            Number.isFinite(params.update.used) &&
            params.update.used >= 0 &&
            Number.isFinite(params.update.size) &&
            params.update.size > 0
          ) {
            const usage: SessionContextUsage = {
              used: params.update.used,
              size: params.update.size,
              estimated: params.update._meta?.estimated === true,
            };
            contextUsageBySessionRef.current.set(params.sessionId, usage);
            if (viewingSessionIdRef.current === params.sessionId) {
              setContextUsage(usage);
            }
          }
          if (!sourceAgentId) {
            let latency = turnLatencyBySessionRef.current.get(
              params.sessionId,
            );
            if (latency && tag === "usage_update") {
              const usageAction = turnUsageActionFromAcp(
                latency.turnId,
                params.update,
              );
              if (usageAction) {
                latency = reduceTurnLatency(latency, usageAction);
                turnLatencyBySessionRef.current.set(
                  params.sessionId,
                  latency,
                );
              }
            }
          }
          const replayed = isReplayedUpdate(params.update);
          if (replayed) {
            reduceReplayedSessionUpdate(
              view,
              params.update,
              sourceAgentId,
            );
          } else {
            // peri 无独立 turn_started 事件：实时内容块到达即视为 turn 进行中。
            // 新一轮开始：先兜底提交上一轮残留的实时文本（保持 history 顺序），
            // 再归约本条更新。
            if (
              tag === "user_message_chunk" &&
              !sourceAgentId &&
              view.status !== "streaming"
            ) {
              commitLiveTurnToHistory(view, {
                thinkingDurationMs:
                  view.turn_started_at != null
                    ? Date.now() - view.turn_started_at
                    : undefined,
              });
              view.turn_started_at = null;
            }
            reduceSessionUpdate(
              view,
              params.update,
              sourceAgentId,
            );
            if (shouldDriveMainSessionStreaming(params.update, sourceAgentId)) {
              view.status = "streaming";
            }
          }
          if (tag === "config_option_update") {
            // 会话级模型恢复（Q1）：session/load 或 set_config_option 后
            // peri 广播当前会话的 model option，同步 composer 的模型显示；
            // 仅当模型仍存在于已配置目录时才应用。
            const modelOption = (params.update.configOptions ?? []).find(
              (option) => (option as { id?: unknown }).id === "model",
            );
            const modelValue = (
              modelOption as { currentValue?: unknown } | undefined
            )?.currentValue;
            if (typeof modelValue === "string" && modelValue.length > 0) {
              modelBySessionRef.current.set(params.sessionId, modelValue);
            }
            if (
              typeof modelValue === "string" &&
              modelValue.length > 0 &&
              viewingSessionIdRef.current === params.sessionId &&
              configuredModelsRef.current.some((m) => m.id === modelValue)
            ) {
              setModelId(modelValue);
            }
          }
          if (
            !replayed &&
            (tag === "agent_message_chunk" ||
              tag === "agent_thought_chunk")
          ) {
            const firstMainTextDelta =
              !sourceAgentId &&
              !hadVisibleMainText &&
              params.update.content.type === "text" &&
              params.update.content.text.trim().length > 0;
            if (firstMainTextDelta) flushProjection(params.sessionId);
            else scheduleProjection(params.sessionId);
          } else {
            flushProjection(params.sessionId);
          }
          };
          if (
            isRequestScopedSessionUpdate(params, sourceAgentId) &&
            activeTurnBootstrap.deferUnknown(
              params.sessionId,
              params.requestId,
              apply,
            )
          ) {
            return;
          }
          apply();
        }),
      );
      registrationPromises.push(
        listenAcp("acp://unstable-event", (notification) => {
          if (disposed) return;
          const params = notification.params;
          if (
            !params?.sessionId ||
            params.event !== "first-provider-event"
          ) {
            return;
          }
          const requestId = params.requestId;
          if (!requestId) return;
          if (
            typeof params.data?.source_agent_id === "string" &&
            params.data.source_agent_id.length > 0
          ) {
            return;
          }
          const sourceAtMs = params.data?.at_ms;
          if (
            typeof sourceAtMs !== "number" ||
            !Number.isFinite(sourceAtMs)
          ) {
            return;
          }
          const latency = turnLatencyBySessionRef.current.get(
            params.sessionId,
          );
          if (!latency || latency.turnId !== requestId) return;
          turnLatencyBySessionRef.current.set(
            params.sessionId,
            reduceTurnLatency(latency, {
              type: "first_sse",
              turnId: latency.turnId,
              atMs: sourceAtMs,
            }),
          );
        }),
      );
      registrationPromises.push(
        listenAcp("acp://agent-event", (notification) => {
          if (disposed) return;
          const params = notification.params;
          if (!params) return;
          const event = parseAgentEvent(params.event_json);
          if (!event) return;
          // OAuth 是 host 级事件且 sessionId 为空；当前提交只建立协议识别，
          // 交互入口由独立 MCP OAuth 功能处理。
          if (!params.sessionId) return;
          const apply = () => {
            const activeRequestId = correlatedTurnId(params.sessionId);
            if (
              !shouldApplyAgentEvent(params, event, activeRequestId)
            ) {
              return;
            }
            const view = ensureAcpSession(
              acpWorkspaceRef.current,
              params.sessionId,
            );
            reduceAgentEvent(view, event);
            if (
              event.type === "turn_suspended" &&
              viewingSessionIdRef.current === params.sessionId
            ) {
              setTurnStartedAt(null);
            }
            flushProjection(params.sessionId);
          };
          if (
            isRequestScopedAgentEvent(event) &&
            activeTurnBootstrap.deferUnknown(
              params.sessionId,
              params.requestId,
              apply,
            )
          ) {
            return;
          }
          apply();
        }),
      );
      registrationPromises.push(
        listenAcp("acp://recovery-status", (notification) => {
          if (disposed) return;
          const params = notification.params;
          if (!params?.session_id) return;
          const view = ensureAcpSession(
            acpWorkspaceRef.current,
            params.session_id,
          );
          reduceRecovery(view, params);
          flushProjection(params.session_id);
        }),
      );
      registrationPromises.push(
        listenAcp("acp://elicitation", (notification) => {
          if (disposed) return;
          const payload = parseElicitationPayload(notification);
          if (!payload) {
            const rpcId = readElicitationRpcId(notification);
            if (rpcId != null) {
              void sessionResolveAskUser({
                rpcId,
                decision: "cancelled",
              }).catch(() => {});
            }
            return;
          }
          const pending = pendingAskUserBySessionRef.current.get(
            payload.sessionId,
          );
          if (pending && pending.rpcId !== payload.rpcId) {
            // 当前弹窗一次只能可靠承载一个表单；立即拒绝并发的新请求，避免静默覆盖。
            void sessionResolveAskUser({
              rpcId: payload.rpcId,
              decision: "cancelled",
            }).catch(() => {});
            return;
          }
          pendingAskUserBySessionRef.current.set(payload.sessionId, payload);
          setPendingAskUserSessionIds((previous) => {
            if (previous.has(payload.sessionId)) return previous;
            const next = new Set(previous);
            next.add(payload.sessionId);
            return next;
          });
          if (viewingSessionIdRef.current === payload.sessionId) {
            setAskUser(payload);
          }
        }),
      );
      registrationPromises.push(
        listenAcp("acp://agent-done", (notification) => {
          if (disposed) return;
          const params = notification.params;
          if (!params?.sessionId || !isForegroundRequestDone(params)) {
            return;
          }
          const apply = () => {
          if (
            completedTurnIdBySessionRef.current.get(params.sessionId) ===
            params.requestId
          ) {
            return;
          }
          const activeTurnId = activeTurnIdBySessionRef.current.get(
            params.sessionId,
          );
          const expectedTurnId = correlatedTurnId(params.sessionId);
          const activeLatency = turnLatencyBySessionRef.current.get(
            params.sessionId,
          );
          if (
            !shouldAcceptAgentDone(expectedTurnId, params.requestId) ||
            (activeLatency && activeLatency.turnId !== params.requestId)
          ) {
            return;
          }
          completedTurnIdBySessionRef.current.set(
            params.sessionId,
            params.requestId,
          );
          if (activeTurnId === params.requestId) {
            activeTurnIdBySessionRef.current.delete(params.sessionId);
          }
          if (
            recoverableCompletedTurnIdBySessionRef.current.get(
              params.sessionId,
            ) === params.requestId
          ) {
            recoverableCompletedTurnIdBySessionRef.current.delete(
              params.sessionId,
            );
          }
          const view = acpWorkspaceRef.current.sessions[params.sessionId];
          const awaitsVisibleToken =
            view?.live_segments.some(
              (segment) =>
                (segment.kind === "thought" ||
                  segment.kind === "content") &&
                segment.text.trim().length > 0,
            ) === true;
          const normalCompletion = isNormalSessionCompletion(
            params.stopReason,
            Boolean(view?.last_error),
          );
          const completedLatency = activeLatency
            ? reduceTurnLatency(activeLatency, {
                type: "completed",
                turnId: activeLatency.turnId,
                atMs: params._keencode.completedAtMs,
              })
            : null;
          const turnMetrics = completedLatency
            ? summarizeTurnLatency(completedLatency)
            : undefined;
          const waitForVisibleCommit = Boolean(
            completedLatency &&
              completedLatency.firstVisibleTokenAtMs == null &&
              awaitsVisibleToken &&
              viewingSessionIdRef.current === params.sessionId,
          );
          if (waitForVisibleCommit && completedLatency) {
            pendingVisibleTurnBySessionRef.current.set(
              params.sessionId,
              completedLatency.turnId,
            );
          } else {
            pendingVisibleTurnBySessionRef.current.delete(params.sessionId);
          }
          if (view) {
            const optimisticUser = (
              messagesBySessionRef.current.get(params.sessionId) ?? []
            )
              .slice()
              .reverse()
              .find(
                (message) =>
                  message.role === "user" && message.id.startsWith("u-"),
              );
            // 完成的实时 Turn 提交进 history，保证转写与自动标题在 turn 边界不丢失。
            commitLiveTurnToHistory(view, {
              userContent: optimisticUser?.content,
              thinkingDurationMs:
                view.turn_started_at != null
                  ? Date.now() - view.turn_started_at
                  : undefined,
              turnMetrics,
              model: optimisticUser?.model,
            });
            view.turn_started_at = null;
            view.status = "idle";
            view.retry = null;
            // 正常完成后计划已失去操作价值；取消、停止与异常保留现场。
            if (normalCompletion) {
              view.todos = {
                revision: view.todos.revision + 1,
                items: [],
              };
            }
          }
          if (
            normalCompletion &&
            viewingSessionIdRef.current !== params.sessionId
          ) {
            setCompletedUnreadIds((previous) => {
              if (previous.has(params.sessionId)) return previous;
              const next = new Set(previous);
              next.add(params.sessionId);
              saveCompletedUnreadSessionIds(next, localStorage);
              return next;
            });
          }
          // 完成通知必须直接清理目标 Session 的后台运行投影，不能依赖当前查看页。
          setLiveMap((previous) =>
            projectHostIntoLiveMap(previous, {
              sessionId: params.sessionId,
              state: "ready",
              streamingMessageId: null,
            }),
          );
          setLiveHost((previous) => {
            if (previous.sessionId !== params.sessionId) return previous;
            const next = {
              ...previous,
              state: "ready" as const,
              streamingMessageId: null,
            };
            liveHostRef.current = next;
            return next;
          });
          if (viewingSessionIdRef.current === params.sessionId) {
            setTurnStartedAt(null);
          }
          if (
            completedLatency &&
            (completedLatency.sendAcknowledgedAtMs == null ||
              waitForVisibleCommit)
          ) {
            // invoke 响应和 Tauri 事件没有跨通道 happens-before。保留已完成
            // 状态，等迟到的 acceptedAtMs 补写历史；它绝不能把回合重开。
            turnLatencyBySessionRef.current.set(
              params.sessionId,
              completedLatency,
            );
          } else {
            turnLatencyBySessionRef.current.delete(params.sessionId);
          }
          clearPendingAskUserRef.current(params.sessionId);
          setAskUser((current) =>
            current?.sessionId === params.sessionId ? null : current,
          );
          if (viewingSessionIdRef.current === params.sessionId) {
            void refreshTaskCacheUsage(params.sessionId);
          }
          flushProjection(params.sessionId);
          };
          const deferred = activeTurnBootstrap.deferUnknown(
            params.sessionId,
            params.requestId,
            apply,
          );
          if (deferred) {
            return;
          }
          apply();
        }),
      );
      const registered = await Promise.all(registrationPromises);
      if (disposed) {
        for (const unlisten of registered) unlisten();
        return;
      }
      unlisteners.push(...registered);
      try {
        const runtimeState = await sessionGetState();
        if (!disposed) {
          const hostActiveTurns = new Map(
            runtimeState.activeTurns.map(({ sessionId, turnId }) => [
              sessionId,
              turnId,
            ]),
          );
          const hostCompletedTurns = new Map(
            runtimeState.completedTurns.map(({ sessionId, turnId }) => [
              sessionId,
              turnId,
            ]),
          );
          const recoverySessionIds = new Set([
            ...recoverableCompletedTurnIdBySessionRef.current.keys(),
            ...hostCompletedTurns.keys(),
            ...hostActiveTurns.keys(),
          ]);
          for (const sessionId of recoverySessionIds) {
            const completedTurnId = hostCompletedTurns.get(sessionId) ?? null;
            const currentTurnId =
              activeTurnIdBySessionRef.current.get(sessionId) ?? null;
            const locallyStartedTurnId =
              currentTurnId &&
              currentTurnId !==
                (activeTurnsBeforeBootstrap.get(sessionId) ?? null)
                ? currentTurnId
                : null;
            if (
              !activeTurnBootstrap.overflowed &&
              !hostActiveTurns.has(sessionId) &&
              !locallyStartedTurnId &&
              completedTurnId &&
              completedTurnIdBySessionRef.current.get(sessionId) !==
                completedTurnId
            ) {
              // Host 已完成但 Tauri done 可能仍在另一通道排队。只允许该精确
              // turn 的尾随事件通过；done handler 随即删除此恢复关联。
              recoverableCompletedTurnIdBySessionRef.current.set(
                sessionId,
                completedTurnId,
              );
            } else {
              recoverableCompletedTurnIdBySessionRef.current.delete(sessionId);
            }
          }
          const sessionIds = new Set([
            ...activeTurnIdBySessionRef.current.keys(),
            ...hostActiveTurns.keys(),
          ]);
          for (const sessionId of sessionIds) {
            const currentTurnId =
              activeTurnIdBySessionRef.current.get(sessionId) ?? null;
            const turnBeforeBootstrap =
              activeTurnsBeforeBootstrap.get(sessionId) ?? null;
            const locallyStartedTurnId =
              currentTurnId && currentTurnId !== turnBeforeBootstrap
                ? currentTurnId
                : null;
            const resolved = resolveActiveTurnFromHostSnapshot({
              snapshotTurnId: hostActiveTurns.get(sessionId) ?? null,
              localTurnId: locallyStartedTurnId,
              completedTurnId:
                completedTurnIdBySessionRef.current.get(sessionId) ?? null,
            });
            if (resolved) {
              activeTurnIdBySessionRef.current.set(sessionId, resolved);
            } else {
              activeTurnIdBySessionRef.current.delete(sessionId);
            }
          }
          activeTurnBootstrap.replayMatching();
          if (activeTurnBootstrap.overflowed) {
            void diagnosticsRecord(
              "frontend.active_turn_bootstrap",
              "恢复窗口事件超过 4096 条，已丢弃溢出事件",
            ).catch(() => {});
          }
          setLiveMap((previous) => {
            let next = previous;
            for (const sessionId of hostCompletedTurns.keys()) {
              if (activeTurnIdBySessionRef.current.has(sessionId)) continue;
              next = projectHostIntoLiveMap(next, {
                sessionId,
                state: "ready",
                streamingMessageId: null,
              });
            }
            for (const sessionId of activeTurnIdBySessionRef.current.keys()) {
              next = projectHostIntoLiveMap(next, {
                sessionId,
                state: "streaming",
                streamingMessageId: null,
              });
            }
            return next;
          });
        }
      } catch {
        activeTurnBootstrap.discard();
        // 后续 sessionConnect 会返回该 Session 的权威 activeTurnId。
      }
    })();
    return () => {
      disposed = true;
      projectionBatcher.cancel();
      for (const unlisten of unlisteners) unlisten();
    };
  }, [commitWorkspace, observeHostActiveTurn, refreshTaskCacheUsage]);

  /**
   * Markdown/Thinking 在非空文本提交到 DOM 后回报；这才是“首可见 Token”，
   * ACP delta 到达监听器本身不再冒充可见时间。
   */
  const handleFirstVisibleToken = useCallback(
    (turnId: string) => {
      const sessionId = session.sessionId;
      if (!sessionId) return;
      const latency = turnLatencyBySessionRef.current.get(sessionId);
      if (!latency || turnId !== latency.turnId) return;
      if (
        latency.completedAtMs != null &&
        pendingVisibleTurnBySessionRef.current.get(sessionId) !== turnId
      ) {
        return;
      }
      const visibleAtMs = turnLatencyNow();
      const visibleLatency = reduceTurnLatency(latency, {
        type: "first_visible_token",
        turnId: latency.turnId,
        atMs: visibleAtMs,
      });
      if (visibleLatency === latency) return;
      pendingVisibleTurnBySessionRef.current.delete(sessionId);
      turnLatencyBySessionRef.current.set(sessionId, visibleLatency);
      if (visibleLatency.completedAtMs == null) return;

      const view = acpWorkspaceRef.current.sessions[sessionId];
      if (
        view &&
        replaceHistoryTurnMetrics(
          view,
          summarizeTurnLatency(visibleLatency),
        )
      ) {
        commitWorkspace();
        applyViewProjectionRef.current(viewingSessionIdRef.current);
      }
      if (visibleLatency.sendAcknowledgedAtMs != null) {
        turnLatencyBySessionRef.current.delete(sessionId);
      }
    },
    [commitWorkspace, session.sessionId],
  );

  // 若完成消息在 commit 前被切走，它从未对用户可见；不允许稍后打开历史时
  // 把“首可见”追补成几分钟后的时间。
  useEffect(() => {
    for (const [sessionId, turnId] of pendingVisibleTurnBySessionRef.current) {
      if (sessionId === session.sessionId) continue;
      pendingVisibleTurnBySessionRef.current.delete(sessionId);
      const latency = turnLatencyBySessionRef.current.get(sessionId);
      if (
        latency?.turnId === turnId &&
        latency.completedAtMs != null &&
        latency.sendAcknowledgedAtMs != null
      ) {
        turnLatencyBySessionRef.current.delete(sessionId);
      }
    }
  }, [session.sessionId]);

  /** 重放扩展事件，并以 ThreadStore 当前消息重建精确的历史顺序。 */
  const replayHistory = useCallback(
    async (sessionId: string, originView?: ViewFocus) => {
      const view = acpWorkspaceRef.current.sessions[sessionId];
      if (!view || view.history.length > 0) return;
      const mayProjectView = () =>
        originView == null ||
        shouldAdoptView(originView, currentViewFocus(), sessionId);
      const restoreStoredHistory = async () => {
        const current = acpWorkspaceRef.current.sessions[sessionId];
        if (!current) return;
        try {
          const [stored, storedSubagents] = await Promise.all([
            sessionMessages(sessionId),
            sessionSubagents(sessionId),
          ]);
          const projected = projectPeriStoredMessages(stored);
          const history = projected.map((message) => ({
            role: message.role,
            content: message.content,
            thought: message.thought,
            segments: message.segments,
          }));
          if (history.length > 0) {
            current.history = history;
            current.subagents = storedSubagents.length
              ? projectPeriStoredSubagentThreads(storedSubagents)
              : projectPeriStoredSubagents(projected);
            current.live_segments = [];
            commitWorkspace();
            if (mayProjectView()) {
              applyViewProjectionRef.current(sessionId);
            }
          }
        } catch {
          /* ignore */
        }
      };
      try {
        const result = await sessionReplay({
          sessionId,
          after: null,
          limit: 500,
        });
        reduceReplayResult(view, result);
        commitWorkspace();
        if (mayProjectView()) {
          applyViewProjectionRef.current(sessionId);
        }
        if (result.replayed_events > 0) {
          // 通知通过窗口事件异步投递；等事件落地后再用 ThreadStore 的真实块顺序覆盖。
          await new Promise((resolve) => window.setTimeout(resolve, 150));
        }
        await restoreStoredHistory();
      } catch {
        await restoreStoredHistory();
      }
    },
    [commitWorkspace, currentViewFocus],
  );

  // Keep refs aligned for event handlers — but not while openSession is loading
  // (otherwise an intermediate null sessionId wipes viewing id and skips UI update).
  useEffect(() => {
    if (openingSessionIdRef.current) return;
    viewingSessionIdRef.current = session.sessionId;
  }, [session.sessionId]);

  // Prompt history is per viewed session — leave browse mode on switch / new chat.
  useEffect(() => {
    promptHistoryIndexRef.current = null;
    setPromptHistoryIndex(null);
    setPromptHistoryOpen(false);
    setPromptHistoryFilter("");
    setPromptHistoryActive(0);
    setPromptHistoryFocusFilter(false);
  }, [session.sessionId]);

  useEffect(() => {
    liveHostRef.current = liveHost;
  }, [liveHost]);

  // 将当前任务消息同步到按 Session 分区的内存缓存。
  useEffect(() => {
    messagesRef.current = messages;
    const id = session.sessionId;
    if (!id) return;
    messagesBySessionRef.current.set(id, messages);
  }, [messages, session.sessionId]);

  /** Apply a message reducer to the viewed session or only to the cache. */
  const patchSessionMessages = useCallback(
    (
      targetSessionId: string | undefined | null,
      reduce: (prev: ChatMessage[]) => ChatMessage[],
    ) => {
      if (!targetSessionId) return;
      if (viewingSessionIdRef.current === targetSessionId) {
        setMessages((prev) => {
          const next = reduce(prev);
          messagesBySessionRef.current.set(targetSessionId, next);
          return next;
        });
      } else {
        const prev = messagesBySessionRef.current.get(targetSessionId) ?? [];
        messagesBySessionRef.current.set(targetSessionId, reduce(prev));
      }
    },
    [],
  );


  const applyThemeChoice = (next: ThemePreference) => {
    saveThemePreference(localStorage, next);
    setThemePreference(next);
    // System: unlock native → re-read OS → set data-theme immediately.
    // Light/dark: lock native + CSS to that value.
    void applyThemePreference(next, {
      onResolved: (resolved, system) => {
        // Always refresh systemTheme so resolveTheme("system", …) is current.
        setSystemTheme(next === "system" ? resolved : system);
      },
    });
  };

  const applySkinChoice = (next: ThemeSkinId) => {
    saveSkin(localStorage, next);
    applySkinToDocument(next);
    setSkin(next);
    const preferred = skinPreferredTheme(next);
    if (preferred && preferred !== theme) {
      applyThemeChoice(preferred);
    }
  };

  const applyWallpaperChoice = async (record: WallpaperRecord | null) => {
    if (!record) {
      try {
        await clearWallpaper();
      } catch (e) {
        showToast(localizeUiError(e, locale), 4000);
        return;
      }
      if (wallpaperUrlRef.current) {
        URL.revokeObjectURL(wallpaperUrlRef.current);
        wallpaperUrlRef.current = null;
      }
      setWallpaperRecord(null);
      setWallpaperUrl(null);
      return;
    }
    // New upload resets focus to cover-center unless the record already has one.
    const toSave: WallpaperRecord = {
      ...record,
      focus: record.focus ?? undefined,
    };
    try {
      await saveWallpaper(toSave);
    } catch (e) {
      showToast(localizeUiError(e, locale), 4000);
      return;
    }
    const url = URL.createObjectURL(toSave.blob);
    if (wallpaperUrlRef.current) URL.revokeObjectURL(wallpaperUrlRef.current);
    wallpaperUrlRef.current = url;
    setWallpaperRecord(toSave);
    setWallpaperUrl(url);
  };

  const applyWallpaperAdjustChoice = async (patch: {
    focus: WallpaperFocus;
    clip: WallpaperClip | null;
    duration?: number;
  }) => {
    try {
      const meta = await saveWallpaperAdjust({
        focus: patch.focus,
        clip: patch.clip,
        duration: patch.duration,
      });
      if (!meta) return;
      setWallpaperRecord((prev) => {
        if (!prev) return prev;
        const next: WallpaperRecord = {
          ...prev,
          focus: meta.focus,
          clip: meta.clip,
        };
        if (!meta.focus) delete next.focus;
        if (!meta.clip) delete next.clip;
        return next;
      });
    } catch (error) {
      setLocalError(localizeUiError(error, locale));
    }
  };

  /** 首次成功读取媒体尺寸后写入元数据，避免后续布局闪动。 */
  const applyWallpaperMediaSize = useCallback(
    async (size: { w: number; h: number }) => {
      try {
        const meta = await saveWallpaperMediaSize(size.w, size.h);
        if (!meta) return;
        setWallpaperRecord((prev) => {
          if (!prev) return prev;
          if (prev.width === meta.width && prev.height === meta.height) return prev;
          return {
            ...prev,
            width: meta.width,
            height: meta.height,
          };
        });
      } catch (error) {
        setLocalError(localizeUiError(error, locale));
      }
    },
    [],
  );

  const applyWallpaperScrimChoice = (value: number) => {
    saveWallpaperScrim(localStorage, value);
    applyWallpaperScrimToDocument(value);
    setWallpaperScrim(value);
  };

  const navigateWorkbench = useCallback(() => {
    setAppView("workbench");
    if (typeof window !== "undefined" && window.location.hash) {
      window.history.replaceState(null, "", window.location.pathname + window.location.search);
    }
  }, []);

  const navigateSettings = useCallback(
    (section: SettingsSectionId = "general") => {
      setSettingsSection(section);
      setAppView("settings");
      if (typeof window !== "undefined") {
        const hash = buildSettingsHash({ section });
        // Avoid no-op hash writes (some webviews skip hashchange; state still set above).
        if (window.location.hash !== hash) {
          window.location.hash = hash;
        }
      }
    },
    [],
  );

  // 哈希路由只接受 #/settings/{section}、#/workbench 或空路径。
  useEffect(() => {
    const syncFromHash = () => {
      const raw = (window.location.hash || "").replace(/^#\/?/, "");
      if (raw.startsWith("settings")) {
        const loc = parseSettingsHash(raw);
        if (loc) {
          setSettingsSection(loc.section);
          setAppView("settings");
        } else {
          setAppView("workbench");
          window.history.replaceState(
            null,
            "",
            window.location.pathname + window.location.search,
          );
        }
      } else if (raw === "" || raw === "workbench") {
        setAppView("workbench");
      }
    };
    syncFromHash();
    window.addEventListener("hashchange", syncFromHash);
    return () => window.removeEventListener("hashchange", syncFromHash);
  }, []);

  /** 连接已存储的 Session，重放 ACP 历史并更新工作台投影。 */
  const openSession = async (s: SessionRow, project?: Project | null) => {
    if (!api.isTauri()) return;
    const proj =
      project ||
      projects.find((p) => p.id === s.projectId) ||
      null;
    setAppView("workbench");
    // 打开即代表用户已经看过该任务的正常完成结果。
    setCompletedUnreadIds((previous) => {
      if (!previous.has(s.id)) return previous;
      const next = new Set(previous);
      next.delete(s.id);
      saveCompletedUnreadSessionIds(next, localStorage);
      return next;
    });

    // User navigation: invalidate any in-flight work that wants the workbench.
    bumpViewEpoch();
    // Snapshot the outgoing thread so a mid-turn switch does not lose the user bubble.
    const leavingId = viewingSessionIdRef.current;
    if (leavingId) {
      messagesBySessionRef.current.set(
        leavingId,
        snapshotOutgoingMessages(
          messagesBySessionRef.current.get(leavingId),
          messagesRef.current,
        ),
      );
    }

    // Point viewing id immediately so late stream chunks land in the right cache.
    openingSessionIdRef.current = s.id;
    viewingSessionIdRef.current = s.id;
    const originView = currentViewFocus();
    openingSessionEpochRef.current = originView.epoch;
    const canAdoptOpenView = () =>
      shouldAdoptView(originView, currentViewFocus(), s.id);
    const ownsOpeningSlot = () =>
      openingSessionIdRef.current === s.id &&
      openingSessionEpochRef.current === originView.epoch;
    const clearOpeningSlot = () => {
      if (!ownsOpeningSlot()) return;
      openingSessionIdRef.current = null;
      openingSessionEpochRef.current = null;
    };
    setAskUser(pendingAskUserBySessionRef.current.get(s.id) ?? null);
    try {
        let hostState: Awaited<ReturnType<typeof sessionConnect>>["state"] | null =
          null;
        let view = acpWorkspaceRef.current.sessions[s.id];
        if (!view) {
          const connected = await sessionConnect({
            projectPath: proj?.path || undefined,
            sessionId: s.id,
          });
          observeHostActiveTurn(connected);
          hostState = connected.state;
          view = ensureAcpSession(acpWorkspaceRef.current, s.id);
          view.project_path = proj?.path ?? null;
          await replayHistory(s.id, originView);
        } else {
          // 已加载的后台 Session 也要显式同步原生端焦点，供提问通知判断当前对话。
          const connected = await sessionConnect({
            projectPath: proj?.path || undefined,
            sessionId: s.id,
          });
          observeHostActiveTurn(connected);
          hostState = connected.state;
          try {
            await replayHistory(s.id, originView);
          } catch {
            const reconnected = await sessionConnect({
              projectPath: proj?.path || undefined,
              sessionId: s.id,
            });
            observeHostActiveTurn(reconnected);
            hostState = reconnected.state;
            view = ensureAcpSession(acpWorkspaceRef.current, s.id);
            view.project_path = proj?.path ?? null;
          }
        }
        if (!view) {
          throw new Error(`ACP Session 未登记：${s.id}`);
        }
        // A slower open may finish after the user has opened B. Keep the ACP
        // connection and workspace data, but never project A over the view.
        if (!canAdoptOpenView()) return;
        const projected = projectAcpSnapshot(view);
        const snapshot = hostState
          ? { ...projected, state: hostState }
          : projected;
        setSession(snapshot);
        setLiveHost(snapshot);
        liveHostRef.current = snapshot;
        setActiveProject(proj);
        setAttachments([]);
        setLocalError(null);
        clearOpeningSlot();
        commitWorkspace();
        applyViewProjection(s.id);
        const sessionModel = modelBySessionRef.current.get(s.id);
        if (
          sessionModel &&
          configuredModelsRef.current.some((model) => model.id === sessionModel)
        ) {
          setModelId(sessionModel);
        }
        await refreshSessions();
    } catch (cause) {
      if (canAdoptOpenView()) {
        setLocalError(localizeUiError(cause, locale));
      }
      clearOpeningSlot();
    }
  };

  // Persist sidebar project collapse (only false entries) after hydrate.
  useEffect(() => {
    if (!expandedProjectsHydratedRef.current) return;
    if (!api.isTauri()) return;
    const ids = collapsedIdsFromExpandMap(expandedProjects);
    void api
      .settingsGet()
      .then((s) => {
        const prev = s.sidebarCollapsedProjectIds;
        if (sameCollapsedIdSet(prev, ids)) return;
        return api.settingsSet({ sidebarCollapsedProjectIds: ids });
      })
      .catch(() => {});
  }, [expandedProjects]);

  /**
   * 在 React 提交后聚焦输入框，直到输入框挂载或达到重试上限。
   */
  const requestComposerFocus = useCallback(() => {
    pendingComposerFocus.current = true;
    const tryFocus = (attemptsLeft: number) => {
      const el = composerInputRef.current;
      if (el && el.getAttribute("contenteditable") !== "false") {
        el.focus({ preventScroll: true });
        resizeComposer(el);
        try {
          const sel = window.getSelection();
          if (sel) {
            const range = document.createRange();
            range.selectNodeContents(el);
            range.collapse(false);
            sel.removeAllRanges();
            sel.addRange(range);
          }
        } catch {
          /* ignore */
        }
        if (document.activeElement === el) {
          pendingComposerFocus.current = false;
          return;
        }
      }
      if (attemptsLeft <= 0) {
        pendingComposerFocus.current = false;
        return;
      }
      requestAnimationFrame(() => tryFocus(attemptsLeft - 1));
    };
    // macOS: button click keeps focus on the button until the next tick.
    window.setTimeout(() => tryFocus(12), 0);
  }, []);

  /**
   * Draft new chat (Codex-style): clear UI only.
   * 首次通过 ensureConnected 成功发送前不创建持久 Session。
   * 无项目任务传入 `null`，任务会显示在“任务”栏目中。
   * Omit / pass undefined to use the active project (requires one).
   */
  const newChat = async (
    project?: Project | null,
    opts?: {
      seedDraft?: string;
    },
  ) => {
    // Explicit null → orphan; undefined → keep active project when set,
    // otherwise orphan draft (no forced "pick a project first").
    const proj = project === undefined ? activeProject : project;
    if (proj && isProjectPathMissing(proj.pathOk)) {
      setLocalError(tr("project.pathMissing", { name: proj.name }));
      return;
    }
    setAppView("workbench");
    setActiveProject(proj);
    if (proj) {
      setExpandedProjects((e) => ({ ...e, [proj.id]: true }));
    } else {
      setHistoryOpen(true);
    }
    // User navigation: a connect/send still in flight for the previous chat must
    // not drag the workbench back here once it resolves.
    bumpViewEpoch();
    // Preserve outgoing thread in cache before clearing the draft UI.
    // Always snapshot current messages (not only if already cached) so a mid-send
    // switch does not drop the optimistic user/assistant bubbles.
    const leavingId = viewingSessionIdRef.current;
    if (leavingId) {
      messagesBySessionRef.current.set(
        leavingId,
        snapshotOutgoingMessages(
          messagesBySessionRef.current.get(leavingId),
          messagesRef.current,
        ),
      );
    }
    viewingSessionIdRef.current = null;
    // Invalidate an in-flight openSession's effect guard as part of navigation.
    openingSessionIdRef.current = null;
    openingSessionEpochRef.current = null;
    setMessages([]);
    setContextUsage(null);
    setDraft(opts?.seedDraft ?? "");
    setAttachments([]);
    sendQueue.clearDraftQueue();
    setAskUser(null);
    setRetryStatus(null);
    setSummaryOpen(false);
    setSession({
      ...IDLE_SNAPSHOT,
      sessionId: null,
      title: tr("session.new"),
      state: "idle",
      backend: "peri_acp",
    });
    setLocalError(null);
    // 新建任务仅切换视图，不中断仍在后台执行的 ACP Session。
    const prevLive = liveHostRef.current;
    if (
      prevLive.sessionId &&
      isSessionLiveStreaming(prevLive.state)
    ) {
      setLiveMap((prev) =>
        projectHostIntoLiveMap(prev, {
          sessionId: prevLive.sessionId,
          state: prevLive.state,
          streamingMessageId: prevLive.streamingMessageId,
        }),
      );
    }
    // Focus explicitly — do not rely only on useEffect: after await, effects may
    // already have run, and identical draft/sessionId can skip a re-render.
    requestComposerFocus();
  };

  const sessionsForProject = (projectId: string) =>
    orderedByIds(sessions.filter(
      (s) => s.projectId === projectId && !s.archived && !s.pinned,
    ), sessionOrder);

  /** 所有项目与无项目任务中的置顶任务。 */
  const pinnedSessions = orderedByIds(sessions.filter((session) => {
    return session.pinned && !session.archived;
  }), sessionOrder);

  const orphanSessions = orderedByIds(sessions.filter(
    (s) =>
      (!s.projectId || !projects.some((p) => p.id === s.projectId)) &&
      !s.archived &&
      !s.pinned,
  ), sessionOrder);

  const startSidebarDrag = (
    event: ReactDragEvent<HTMLElement>,
    kind: "project" | "session",
    id: string,
  ) => {
    draggedSidebarItemRef.current = { kind, id };
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", id);
  };

  const endSidebarDrag = () => {
    draggedSidebarItemRef.current = null;
    setProjectDropHint(null);
  };

  const applyProjectOrder = (ids: string[]) => {
    if (ids.every((id, index) => id === projects[index]?.id)) return;
    setProjects(orderedByIds(projects, ids));
    const revision = ++projectReorderRevisionRef.current;
    projectReorderQueueRef.current = projectReorderQueueRef.current.then(async () => {
      try {
        const saved = (await api.projectsReorder(ids)) as Project[];
        if (revision === projectReorderRevisionRef.current) setProjects(saved);
      } catch (error) {
        if (revision !== projectReorderRevisionRef.current) return;
        await refreshProjects();
        setLocalError(localizeUiError(error, locale));
      }
    });
  };

  const dragOverProject = (
    event: ReactDragEvent<HTMLElement>,
    targetId: string,
  ) => {
    if (draggedSidebarItemRef.current?.kind !== "project") return;
    event.preventDefault();
    const { top, height } = event.currentTarget.getBoundingClientRect();
    const after = event.clientY > top + height / 2;
    setProjectDropHint((current) =>
      current?.id === targetId && current.after === after
        ? current
        : { id: targetId, after },
    );
  };

  const dropProject = (event: ReactDragEvent<HTMLElement>, targetId: string) => {
    event.preventDefault();
    event.stopPropagation();
    const dragged = draggedSidebarItemRef.current;
    if (dragged?.kind !== "project") return;
    const { top, height } = event.currentTarget.getBoundingClientRect();
    const ids = moveId(
      projects.map(({ id }) => id),
      dragged.id,
      targetId,
      event.clientY > top + height / 2,
    );
    draggedSidebarItemRef.current = null;
    setProjectDropHint(null);
    applyProjectOrder(ids);
  };

  const dropSession = (event: ReactDragEvent<HTMLElement>, targetId: string) => {
    event.preventDefault();
    event.stopPropagation();
    const dragged = draggedSidebarItemRef.current;
    if (dragged?.kind !== "session") return;
    const { top, height } = event.currentTarget.getBoundingClientRect();
    const ids = moveId(
      orderedByIds(sessions, sessionOrder).map(({ id }) => id),
      dragged.id,
      targetId,
      event.clientY > top + height / 2,
    );
    draggedSidebarItemRef.current = null;
    setSessionOrder(ids);
    saveSessionOrder(ids);
  };

  /**
   * 多会话忙碌标识，用于侧栏运行中状态。
   * Uses liveMap projection + liveHost fallback. Excludes connecting.
   */
  const busyIds = useMemo(() => {
    const set = busySessionIds(liveMap);
    if (liveHost.sessionId && isSessionLiveStreaming(liveHost.state)) {
      set.add(liveHost.sessionId);
    }
    return set;
  }, [liveMap, liveHost.sessionId, liveHost.state]);
  const effectiveCanSend = canSendWithStopLatch(session.state, stopLatch);
  const effectiveCanStop = canStopWithStopLatch(session.state, stopLatch);

  const refreshSessions = async () => {
    try {
      if (!api.isTauri()) return;
      const [rows, persistedProjects] = await Promise.all([
        sessionsList(),
        api.projectsList() as Promise<Project[]>,
      ]);
      const projection = projectSidebar(
        rows,
        loadSessionPreferences(),
        persistedProjects,
      );
      setProjects(projection.projects);
      setSessions(projection.sessions);
    } catch {
      /* ignore */
    }
  };

  const refreshProjects = async () => {
    try {
      const list = await api.projectsList();
      const mapped = list as Project[];
      setProjects(mapped);
      // Keep active project pathOk/path in sync with Host re-check.
      setActiveProject((prev) => {
        if (!prev) return prev;
        return mapped.find((x) => x.id === prev.id) ?? prev;
      });
    } catch {
      /* ignore */
    }
  };

  const applySessionTitle = useCallback(
    (sessionId: string, title: string) => {
      sessionTitleOverridesRef.current.set(sessionId, title);
      setSessions((list) =>
        list.map((s) => (s.id === sessionId ? { ...s, title } : s)),
      );
      setSession((prev) =>
        prev.sessionId === sessionId ? { ...prev, title } : prev,
      );
    },
    [],
  );

  const renameProject = (proj: Project) => {
    setCtxMenu(null);
    setAppDialog({
      kind: "prompt",
      title: tr("project.rename"),
      initial: proj.name,
      onSubmit: async (name) => {
        const next = name.trim();
        if (!next || next === proj.name) return;
        try {
          await api.projectRename(proj.id, next);
          await refreshProjects();
          if (activeProject?.id === proj.id) {
            setActiveProject((p) => (p ? { ...p, name: next } : p));
          }
        } catch (e) {
          setLocalError(localizeUiError(e, locale));
        }
      },
    });
  };

  /** 打开任务标题重命名弹窗并持久化到 peri 与 KeenCode 本地偏好。 */
  const renameSession = (target: SessionRow) => {
    setCtxMenu(null);
    setAppDialog({
      kind: "prompt",
      title: tr("session.renamePrompt"),
      initial: target.title,
      placeholder: tr("session.renamePlaceholder"),
      onSubmit: async (name) => {
        const next = name.trim();
        if (!next || next === target.title) return;
        try {
          await acpSessionRename(target.id, next);
          updateSessionPreference(target.id, {
            title: next,
            titleSource: "manual",
          });
          applySessionTitle(target.id, next);
          await refreshSessions();
        } catch (error) {
          setLocalError(localizeUiError(error, locale));
        }
      },
    });
  };

  /**
   * Pick a new folder for a project whose path is gone or moved (D05).
   * Host persists path and re-checks is_dir → pathOk true.
   */
  const relocateProject = async (proj: Project) => {
    setCtxMenu(null);
    if (!api.isTauri()) {
      setLocalError(tr("error.needTauri"));
      return;
    }
    try {
      const dir = await api.pickDirectory();
      if (!dir) return;
      const updated = (await api.projectRelocate(proj.id, dir)) as Project;
      await refreshProjects();
      if (activeProject?.id === proj.id) {
        setActiveProject(updated);
        // Force reconnect on next send — cwd changed.
        setSession((prev) =>
          prev.sessionId
            ? {
                ...IDLE_SNAPSHOT,
                sessionId: prev.sessionId,
                title: prev.title,
                state: "idle",
                backend: "peri_acp",
              }
            : prev,
        );
        setLiveHost((prev) =>
          prev.sessionId ? { ...IDLE_SNAPSHOT } : prev,
        );
      }
      setLocalError(null);
      const msg = tr("project.relocateOk", {
        name: updated.name,
        path: updated.path,
      });
      setToast(msg);
      window.setTimeout(
        () => setToast((cur) => (cur === msg ? null : cur)),
        3200,
      );
    } catch (e) {
      setLocalError(localizeUiError(e, locale));
    }
  };

  /** Remove project from app list only (disk folder + chats kept). */
  const removeProjectFromApp = (proj: Project) => {
    setCtxMenu(null);
    setAppDialog({
      kind: "confirm",
      title: tr("project.removeTitle"),
      message: tr("project.removeConfirmDetail", { name: proj.name }),
      confirmLabel: tr("project.remove"),
      danger: true,
      onConfirm: async () => {
        try {
          if (!api.isTauri()) {
            setLocalError(tr("error.needTauri"));
            return;
          }
          await api.projectRemove(proj.id);
          setVisibleSessionsByProject((counts) =>
            Object.fromEntries(
              Object.entries(counts).filter(([id]) => id !== proj.id),
            ),
          );
          if (activeProject?.id === proj.id) {
            setActiveProject(null);
            setSession(IDLE_SNAPSHOT);
            setMessages([]);
            setContextUsage(null);
            setAskUser(null);
          }
          await refreshProjects();
          await refreshSessions();
          setLocalError(null);
        } catch (e) {
          setLocalError(localizeUiError(e, locale));
        }
      },
    });
  };

  /**
   * Archive / unarchive a session.
   * If the open conversation is archived, leave it for a fresh draft so the
   * main pane does not keep showing a chat that disappeared from the tree.
   */
  const archiveSession = async (s: SessionRow, archived = true) => {
    setCtxMenu(null);
    const wasViewing =
      archived &&
      (session.sessionId === s.id || viewingSessionIdRef.current === s.id);
    try {
      updateSessionPreference(s.id, { archived });
      await refreshSessions();
      if (wasViewing) {
        const proj = s.projectId
          ? projects.find((p) => p.id === s.projectId) ?? null
          : null;
        // 尽量保留相同项目上下文；无项目任务则创建到“任务”栏目。
        if (proj) await newChat(proj);
        else await newChat(null);
      } else if (!archived && s.projectId) {
        setExpandedProjects((e) => ({ ...e, [s.projectId!]: true }));
      }
    } catch (e) {
      setLocalError(localizeUiError(e, locale));
    }
  };

  /** Pin / unpin a session (floats to top of its sidebar group). */
  const pinSession = async (s: SessionRow, pinned = true) => {
    setCtxMenu(null);
    try {
      updateSessionPreference(s.id, { pinned });
      await refreshSessions();
    } catch (e) {
      setLocalError(localizeUiError(e, locale));
    }
  };

  const copySessionId = async (s: SessionRow) => {
    setCtxMenu(null);
    try {
      await navigator.clipboard.writeText(s.id);
    } catch {
      setLocalError(s.id);
    }
  };

  /** 会话菜单「查看轨迹」：展开右侧停靠栏并定位到该会话的台账。 */
  const viewTrajectory = (s: SessionRow) => {
    setCtxMenu(null);
    setLayout((l) => {
      const n = { ...l, asideCollapsed: false };
      saveLayout(localStorage, n);
      return n;
    });
    setResourceOpenTarget({
      type: "trajectory",
      sessionId: s.id,
      title: s.title,
    });
  };

  /** 轨迹台账的数据源：内存缓存优先，其次回放持久化消息。 */
  const loadTrajectoryMessages = useCallback(
    async (id: string): Promise<ChatMessage[]> => {
      const cached = messagesBySessionRef.current.get(id);
      if (cached?.length) return cached;
      try {
        return projectPeriStoredMessages(await sessionMessages(id));
      } catch {
        return [];
      }
    },
    [],
  );

  const openSessionMenu = (e: ReactMouseEvent, s: SessionRow) => {
    e.preventDefault();
    e.stopPropagation();
    setCtxMenu({ kind: "session", id: s.id, x: e.clientX, y: e.clientY });
  };

  const openProjectMenu = (e: ReactMouseEvent, proj: Project) => {
    e.preventDefault();
    e.stopPropagation();
    setCtxMenu({ kind: "project", id: proj.id, x: e.clientX, y: e.clientY });
  };

  const searchHits = useMemo(
    () =>
      filterSessionSearch(
        searchQuery,
        sessions.map((s) => ({
          id: s.id,
          title: s.title,
          projectId: s.projectId,
          archived: s.archived,
        })),
        projects.map((p) => ({ id: p.id, name: p.name, path: p.path })),
      ),
    [searchQuery, sessions, projects],
  );

  /**
   * 用户发出首条消息后，立即用消息开头作为会话默认标题；
   * 独立标题模型随后只根据首条用户消息生成短标题并覆盖。
   * 手动命名或已生成标题的会话不会被动。
   */
  const applyMessagePrefixTitle = useCallback(
    (sessionId: string, userText: string) => {
      const source = loadSessionPreferences()[sessionId]?.titleSource;
      if (
        source === "manual" ||
        source === "automatic" ||
        source === "message-prefix"
      ) {
        return;
      }
      const title = buildSessionTitleFromFirstMessage([
        { role: "user", content: userText },
      ]);
      if (!title) return;
      const currentTitle =
        sessionTitleOverridesRef.current.get(sessionId) ??
        sessionsRef.current.find((row) => row.id === sessionId)?.title ??
        null;
      if (
        !isPlaceholderSessionTitle(currentTitle, [
          tr("session.new"),
          tr("session.placeholderTitle"),
          tr("session.untitled"),
        ])
      ) {
        return;
      }
      updateSessionPreference(sessionId, {
        title,
        titleSource: "message-prefix",
      });
      applySessionTitle(sessionId, title);
      acpSessionRename(sessionId, title).catch((error) =>
        console.warn("persist message-prefix session title failed", error),
      );
    },
    [applySessionTitle, tr],
  );

  /** 使用首条用户消息独立请求语义化短标题，不等待 Assistant 回复。 */
  const applyAutomaticSessionTitle = useCallback(
    async (
      sessionId: string,
      firstUserMessage: string,
      expectedTitle?: string | null,
    ): Promise<void> => {
      if (
        autoTitleAttemptedRef.current.has(sessionId) ||
        autoTitleInFlightRef.current.has(sessionId)
      ) {
        return;
      }
      const currentTitle =
        sessionTitleOverridesRef.current.get(sessionId) ??
        sessionsRef.current.find((row) => row.id === sessionId)?.title ??
        expectedTitle;
      const preferences = loadSessionPreferences()[sessionId];
      const canReplaceCurrentTitle = canGenerateAutomaticSessionTitle({
        currentTitle,
        titleSource: preferences?.titleSource,
        localizedPlaceholders: [
          tr("session.new"),
          tr("session.placeholderTitle"),
          tr("session.untitled"),
        ],
      });
      if (!canReplaceCurrentTitle) return;

      autoTitleAttemptedRef.current.add(sessionId);
      autoTitleInFlightRef.current.add(sessionId);
      try {
        const candidate = await sessionGenerateTitle({
          id: sessionId,
          userMessage: firstUserMessage,
        });
        const title = sanitizeGeneratedSessionTitle(candidate);
        if (!title) return;

        const latestPreferences = loadSessionPreferences()[sessionId];
        const latestTitle =
          sessionTitleOverridesRef.current.get(sessionId) ??
          sessionsRef.current.find((row) => row.id === sessionId)?.title ??
          expectedTitle;
        const canReplaceLatestTitle = canGenerateAutomaticSessionTitle({
          currentTitle: latestTitle,
          titleSource: latestPreferences?.titleSource,
          localizedPlaceholders: [
            tr("session.new"),
            tr("session.placeholderTitle"),
            tr("session.untitled"),
          ],
        });
        if (!canReplaceLatestTitle) return;

        updateSessionPreference(sessionId, {
          title,
          titleSource: "automatic",
        });
        applySessionTitle(sessionId, title);
        try {
          await acpSessionRename(sessionId, title);
        } catch (error) {
          console.warn("persist generated session title failed", error);
        }
      } catch (error) {
        console.warn("generate session title failed", error);
      } finally {
        autoTitleInFlightRef.current.delete(sessionId);
      }
    },
    [applySessionTitle, tr],
  );

  /**
   * 兜底：非发送路径（如历史重放）进入首条消息时，同样立即应用消息前缀标题。
   */
  useEffect(() => {
    if (!api.isTauri() || !acpSessionView) return;
    const sessionId = acpSessionView.session_id;
    const firstUserText = extractFirstUserMessageText(acpSessionView.history);
    if (!firstUserText) return;
    applyMessagePrefixTitle(sessionId, firstUserText);
    void applyAutomaticSessionTitle(
      sessionId,
      firstUserText,
      acpSessionView.title ?? null,
    );
  }, [
    acpSessionView,
    applyAutomaticSessionTitle,
    applyMessagePrefixTitle,
  ]);

  /**
   * 确保 Session 已创建并连接到进程内 ACP 运行时。
   * Creates store session only on first send (draft → real).
   * Reconnects when disconnected / crashed. Pass force to tear down a "ready"
   * session that may be wedged (e.g. after a timeout).
   * Returns the live session id when ready, else null.
   *
   * Prefer `opts.sessionId` (e.g. queue flush target) over the render-time
   * `session` closure so connect never binds the wrong chat after a switch.
   *
   * Does not yank the UI if the user already switched to another session while
   * connect is in flight; still updates liveHost so the sidebar spinner tracks work.
   */
  const ensureConnected = async (
    forceOrOpts:
      | boolean
      | { force?: boolean; sessionId?: string | null } = false,
  ): Promise<string | null> => {
    const opts =
      typeof forceOrOpts === "boolean"
        ? { force: forceOrOpts, sessionId: undefined as string | null | undefined }
        : forceOrOpts;
    const force = !!opts.force;
    // Explicit target wins; else the session this render is bound to.
    const preferredId =
      opts.sessionId !== undefined ? opts.sessionId : session.sessionId;

    // Project-less (orphan) sessions are allowed: cwd falls back on Host.
    if (activeProject && isProjectPathMissing(activeProject.pathOk)) {
      setLocalError(
        tr("project.pathMissing", { name: activeProject.name }),
      );
      return null;
    }
    const originView = currentViewFocus();
    if (api.isTauri()) {
      if (connectingRef.current) return null;
      connectingRef.current = true;
      setConnecting(true);
      try {
        const existing = preferredId
          ? acpWorkspaceRef.current.sessions[preferredId]
          : undefined;
        // force 语义：已有视图也重新 attach（后端处理重连）；否则直接复用。
        if (existing && !force) {
          return preferredId ?? existing.session_id;
        }

        const draftMessages =
          preferredId == null
            ? messagesBySessionRef.current.get("__draft__")
            : undefined;
        const opened = await sessionConnect({
          projectPath: activeProject?.path || undefined,
          sessionId: preferredId ?? null,
        });
        const session_id = opened.sessionId ?? null;
        if (!session_id) {
          throw new Error("session_connect 未返回 sessionId");
        }
        observeHostActiveTurn(opened);
        const view = ensureAcpSession(acpWorkspaceRef.current, session_id);
        if (!preferredId) {
          await sessionSetEffort({ sessionId: session_id, effort });
        }
        if (draftMessages?.length) {
          messagesBySessionRef.current.set(session_id, draftMessages);
          if (
            messagesBySessionRef.current.get("__draft__") === draftMessages
          ) {
            messagesBySessionRef.current.delete("__draft__");
          }
        }
        void replayHistory(session_id, originView);
        view.project_path = opened.projectPath ?? null;
        const snapshot = {
          ...projectAcpSnapshot(view),
          state: opened.state,
        };
        setLiveHost(snapshot);
        liveHostRef.current = snapshot;
        commitWorkspace();
        if (shouldAdoptView(originView, currentViewFocus(), session_id)) {
          viewingSessionIdRef.current = session_id;
          setSession(snapshot);
          setLocalError(null);
          applyViewProjection(session_id);
        }
        await refreshSessions();
        return session_id;
      } catch (cause) {
        if (
          (preferredId != null &&
            viewingSessionIdRef.current === preferredId) ||
          isSameView(originView, currentViewFocus())
        ) {
          setLocalError(localizeUiError(cause, locale));
        }
        return null;
      } finally {
        connectingRef.current = false;
        setConnecting(false);
      }
    }
    return null;
  };

  const attachLabels = useMemo(
    () => ({
      open: tr("attach.open"),
      reveal: tr("attach.reveal"),
      copyPath: tr("attach.copyPath"),
      copyImage: tr("attach.copyImage"),
      addToComposer: tr("attach.addToComposer"),
      remove: tr("composer.attachRemove"),
      viewImage: tr("image.view"),
    }),
    [tr],
  );

  /**
   * Dispatch one user turn (optimistic UI + connect + session_send).
   * @param targetSessionId When set (queue flush), bind optimistic UI to this id.
   * @param fromQueue Drop user+assistant on failure so requeue does not duplicate.
   */
  const executeSend = async (opts: {
    storedDisplay: string;
    att: Attachment[];
    createGoal?: boolean;
    /** 计划模式：本轮发送注入规划契约（持久开关，不随发送清除）。 */
    planMode?: boolean;
    /** Ultra：本轮主动使用 KeenCode 单层 Agent 委派策略。 */
    ultraMode?: boolean;
    fromQueue?: boolean;
    targetSessionId?: string | null;
  }): Promise<boolean> => {
    if (sendInFlightRef.current) return false;
    sendInFlightRef.current = true;
    const {
      storedDisplay,
      att,
      createGoal = false,
      planMode = false,
      ultraMode = false,
      fromQueue,
    } = opts;
    const segments = parseStoredContent(storedDisplay);
    if (isDraftEmpty(segments) && !att.length) {
      sendInFlightRef.current = false;
      return false;
    }
    if (!hasConfiguredModel) {
      sendInFlightRef.current = false;
      return false;
    }
    const sendTargetId =
      opts.targetSessionId !== undefined
        ? opts.targetSessionId
        : session.sessionId;
    const cacheKey = sendTargetId ?? "__draft__";
    // Draft sends have no id to compare, so pin them to the view they came from:
    // otherwise the optimistic bubbles / streaming state paint whatever *new*
    // draft the user opened in the meantime.
    const originView = currentViewFocus();
    const viewingTarget = () =>
      isViewingSendTarget(originView, currentViewFocus(), sendTargetId);

    const agentBody = serializeForAgent(segments);
    const agentText = buildAgentPrompt(agentBody, att);
    // contenteditable 会在末尾插入占位 <br>；乐观消息必须展示实际发送后的边界，
    // 否则回合完成前会多出空白行，手动复制整条消息时也会带上尾随换行。
    const optimisticDisplay = storedDisplay.trim();
    const turnStartedAtMs = turnLatencyNow();
    const ts = Math.floor(turnStartedAtMs);
    const userMessageId = `u-${ts}`;
    const pendingAssistantId = `a-pending-${ts}`;
    const requestId = globalThis.crypto.randomUUID();
    const dropIds = fromQueue
      ? new Set([userMessageId, pendingAssistantId])
      : new Set([pendingAssistantId]);
    const stripOptimistic = (m: ChatMessage[]) =>
      m.filter((x) => !dropIds.has(x.id));

    if (viewingTarget()) setRetryStatus(null);
    const nowIso = new Date().toISOString();
    const appendOptimistic = (m: ChatMessage[]): ChatMessage[] => {
      const cleaned = clearPriorTurnErrors(clearPriorTurnStreaming(m));
      return [
        ...cleaned,
        {
          id: userMessageId,
          role: "user",
          content: optimisticDisplay,
          model: modelLabel,
          attachments: att.length ? att : undefined,
          createdAt: nowIso,
        },
        {
          id: pendingAssistantId,
          role: "assistant",
          content: "",
          streaming: true,
        },
      ];
    };
    if (sendTargetId) {
      patchSessionMessages(sendTargetId, appendOptimistic);
    } else if (viewingTarget()) {
      setMessages((m) => {
        const next = appendOptimistic(m);
        messagesBySessionRef.current.set(cacheKey, next);
        return next;
      });
    } else {
      const prev = messagesBySessionRef.current.get(cacheKey) ?? [];
      messagesBySessionRef.current.set(cacheKey, appendOptimistic(prev));
    }
    // 回合计时从发送时刻开始，不依赖焦点状态或首个 token 到达。
    setTurnStartedAt(ts);
    if (viewingTarget()) {
      setSession((prev) =>
        prev.state === "streaming"
          ? prev
          : { ...prev, state: "streaming", lastError: null },
      );
    }
    // Optimistic liveHost only when we already own the live slot (or nothing is live).
    // Never stamp streaming onto a foreign mid-turn — ensureConnected demotes first.
    setLiveHost((prev) => {
      if (prev.sessionId) {
        if (sendTargetId && prev.sessionId !== sendTargetId) return prev;
        // Draft / null target while another session is live → leave Host alone.
        if (!sendTargetId && prev.sessionId) return prev;
      }
      const next = {
        ...prev,
        sessionId: sendTargetId ?? prev.sessionId,
        state: "streaming" as const,
        lastError: null,
      };
      liveHostRef.current = next;
      return next;
    });

    const failStrip = () => {
      if (sendTargetId) {
        patchSessionMessages(sendTargetId, stripOptimistic);
      } else {
        const draftMsgs = messagesBySessionRef.current.get("__draft__");
        if (draftMsgs) {
          messagesBySessionRef.current.set(
            "__draft__",
            stripOptimistic(draftMsgs),
          );
        }
        if (viewingTarget()) setMessages((m) => stripOptimistic(m));
      }
      if (viewingTarget()) {
        setSession((prev) =>
          prev.state === "streaming"
            ? { ...prev, state: prev.sessionId ? "ready" : prev.state }
            : prev,
        );
      }
      // Symmetric rollback of optimistic liveHost streaming — otherwise
      // useSendQueue.flush sees streaming forever and auto-flush starves.
      // 不回退未被当前发送流程接管的其他任务。
      setLiveHost((prev) => {
        if (prev.sessionId) {
          if (sendTargetId && prev.sessionId !== sendTargetId) return prev;
          if (!sendTargetId && prev.sessionId) return prev;
        }
        if (prev.state !== "streaming") return prev;
        const next = {
          ...prev,
          state: (prev.sessionId ? "ready" : "idle") as SessionSnapshot["state"],
        };
        liveHostRef.current = next;
        return next;
      });
    };

    let latencySessionId: string | null = null;
    try {
      let sessionId: string | null = null;
      const live = liveHostRef.current;
      if (
        sendTargetId &&
        live.sessionId === sendTargetId &&
        live.state === "ready" &&
        !live.lastError
      ) {
        sessionId = sendTargetId;
      } else if (
        fromQueue &&
        sendTargetId &&
        viewingSessionIdRef.current !== sendTargetId
      ) {
        failStrip();
        return false;
      } else {
        sessionId = await ensureConnected({ sessionId: sendTargetId });
      }
      if (!sessionId) {
        failStrip();
        return false;
      }
      if (fromQueue && sendTargetId && sessionId !== sendTargetId) {
        failStrip();
        return false;
      }
      // Bind draft message cache to the real id early (Host already materialized).
      // Queue binding waits until sessionSend succeeds so a failed flush can
      // requeue under the original claim key (`__draft__`) without splitting.
      if (!sendTargetId) {
        const draftMsgs = messagesBySessionRef.current.get("__draft__");
        if (draftMsgs?.length) {
          messagesBySessionRef.current.set(sessionId, draftMsgs);
          messagesBySessionRef.current.delete("__draft__");
        }
        // 草稿首发建立的会话继承计划模式开关，避免模式在会话实体化时静默失效。
        if (planMode) {
          setPlanModeSessionKey(sessionId);
        }
        if (ultraMode) {
          setUltraModeSessionKey(sessionId);
        }
      }
      if (
        fromQueue &&
        sendTargetId &&
        liveHostRef.current.sessionId &&
        liveHostRef.current.sessionId !== sendTargetId
      ) {
        failStrip();
        return false;
      }
      const existingActiveTurnId = activeTurnIdBySessionRef.current.get(
        sessionId,
      );
      if (
        existingActiveTurnId &&
        existingActiveTurnId !== requestId
      ) {
        throw new Error("Session 正在运行，当前消息不能覆盖已有回合");
      }
      const acpView = ensureAcpSession(acpWorkspaceRef.current, sessionId);
      beginLocalSessionTurn(acpView, ts);
      pendingVisibleTurnBySessionRef.current.delete(sessionId);
      recoverableCompletedTurnIdBySessionRef.current.delete(sessionId);
      activeTurnIdBySessionRef.current.set(sessionId, requestId);
      // 清除上一轮错误后立即重建当前投影。首次草稿发送的 connect/replay
      // 可能已经把旧 last_error 重新投影到消息列表；不能等首个 ACP chunk 才修正。
      commitWorkspace();
      if (viewingSessionIdRef.current === sessionId) {
        applyViewProjectionRef.current(sessionId);
      }
      turnLatencyBySessionRef.current.set(
        sessionId,
        createTurnLatencyState(requestId, turnStartedAtMs),
      );
      latencySessionId = sessionId;
      if (createGoal) {
        const objective = agentBody.trim();
        if (!objective) {
          throw new Error(tr("goal.objectiveRequired"));
        }
        const result = await goalUpsert({
          sessionId,
          goal: { title: objective, description: objective },
        });
        const view = ensureAcpSession(acpWorkspaceRef.current, sessionId);
        view.goal = { revision: result.revision, goal: result.goal };
        commitWorkspace();
      }
      // Session 一旦建立就立即用首条消息更新标题；不等待请求往返或首个模型事件。
      applyMessagePrefixTitle(sessionId, optimisticDisplay);
      // 前缀标题落地后立即独立请求语义化短标题，不依赖主请求成功或 Assistant 回复。
      void applyAutomaticSessionTitle(sessionId, optimisticDisplay);
      // 首次发送时 sessionConnect 会投影一次 ready 快照，不能让它覆盖发送按钮
      // 已经建立的 streaming 状态；Host 的 accepted 响应只负责确认已接管回合。
      if (
        viewingSessionIdRef.current === sessionId ||
        viewingTarget()
      ) {
        setSession((previous) => ({
          ...previous,
          sessionId,
          state: "streaming",
          lastError: null,
        }));
      }
      setLiveHost((previous) => {
        const next = {
          ...previous,
          sessionId,
          state: "streaming" as const,
          lastError: null,
        };
        liveHostRef.current = next;
        return next;
      });
      // Bind the turn to `sessionId`, never to "whatever is live". Host
      // re-focuses that chat (background/parked → live) before prompting, so a
      // warm connect racing this send cannot deliver it to another chat — and
      // a mid-send "new chat" still lets this turn complete.
      const accepted = await sessionSend({
        text: agentText,
        sessionId,
        requestId,
        planMode,
        ultraMode,
      });
      if (accepted.activeTurnId !== requestId) {
        throw new Error("Host 返回了不匹配的 requestId");
      }
      const latency = turnLatencyBySessionRef.current.get(sessionId);
      if (latency?.turnId === requestId) {
        const acknowledgedLatency = reduceTurnLatency(latency, {
          type: "send_acknowledged",
          turnId: latency.turnId,
          atMs: accepted.acceptedAtMs,
        });
        if (acknowledgedLatency.completedAtMs != null) {
          // 极早完成先于 invoke 响应：只补写指标，保持 done 已投影的 ready，
          // 不能应用 accepted 快照中接受时刻的旧 streaming 状态。
          const view = acpWorkspaceRef.current.sessions[sessionId];
          if (
            view &&
            replaceHistoryTurnMetrics(
              view,
              summarizeTurnLatency(acknowledgedLatency),
            )
          ) {
            commitWorkspace();
            applyViewProjectionRef.current(viewingSessionIdRef.current);
          }
          if (
            pendingVisibleTurnBySessionRef.current.get(sessionId) ===
            acknowledgedLatency.turnId
          ) {
            turnLatencyBySessionRef.current.set(
              sessionId,
              acknowledgedLatency,
            );
          } else {
            turnLatencyBySessionRef.current.delete(sessionId);
          }
        } else {
          turnLatencyBySessionRef.current.set(
            sessionId,
            acknowledgedLatency,
          );
          // Host 只确认已接管回合；模型仍在后台运行。保持目标 Session busy，
          // 直到 acp://agent-done 收敛为 ready。
          setLiveMap((prev) =>
            projectHostIntoLiveMap(prev, {
              sessionId,
              state: accepted.state,
              streamingMessageId: null,
            }),
          );
        }
      } else {
        // 当前 Session 已进入另一回合或被断开；迟到的 accepted 不能覆盖新状态。
      }
      // Only after a successful send: move remaining draft follow-ups onto the
      // real session. If this threw, claim requeues under `__draft__` intact.
      if (!sendTargetId) {
        sendQueue.bindDraft(sessionId);
      }
      return true;
    } catch (e) {
      if (latencySessionId) {
        const latency = turnLatencyBySessionRef.current.get(latencySessionId);
        if (latency?.turnId === requestId) {
          turnLatencyBySessionRef.current.delete(latencySessionId);
        }
        if (
          activeTurnIdBySessionRef.current.get(latencySessionId) ===
          requestId
        ) {
          activeTurnIdBySessionRef.current.delete(latencySessionId);
          const view = acpWorkspaceRef.current.sessions[latencySessionId];
          if (view) {
            view.status = "idle";
            view.turn_started_at = null;
            view.retry = null;
            commitWorkspace();
            if (viewingSessionIdRef.current === latencySessionId) {
              applyViewProjectionRef.current(latencySessionId);
            }
          }
        }
      }
      failStrip();
      if (viewingTarget()) setLocalError(localizeUiError(e, locale));
      return false;
    } finally {
      sendInFlightRef.current = false;
    }
  };

  const clearComposerAfterSubmit = () => {
    setDraft("");
    setGoalModeSessionKey(null);
    promptHistoryIndexRef.current = null;
    setPromptHistoryIndex(null);
    setPromptHistoryOpen(false);
    setPromptHistoryFilter("");
    setPromptHistoryActive(0);
    setPromptHistoryFocusFilter(false);
    setSlashQuery(null);
    setAttachments([]);
    requestAnimationFrame(() => {
      const el = document.querySelector<HTMLElement>(".composer__input");
      if (el) el.style.height = "auto";
    });
  };

  /** Enqueue when agent is busy; otherwise send immediately. */
  const send = async () => {
    const goalModeSelected =
      goalModeSessionKey === (session.sessionId ?? "__draft__");
    // 计划模式是会话级持久开关：与 goal 的一次性语义不同，不随发送清除。
    const planModeSelected =
      planModeSessionKey === (session.sessionId ?? "__draft__");
    const ultraModeSelected =
      ultraModeSessionKey === (session.sessionId ?? "__draft__");
    const storedDisplay = draft;
    const segments = parseStoredContent(storedDisplay);
    const att = attachments;
    if (isDraftEmpty(segments) && !att.length) return;
    if (!hasConfiguredModel) return;
    sendQueue.releaseFlushHold();

    // Enqueue only when *this viewed chat* is busy/connecting (follow-ups).
    // Host mid-turn on another session → executeSend demotes & spawns concurrent
    // work. Never park a new-chat / other-session send into a fake local queue
    // (that showed “本会话队列” on empty welcome while the real turn ran elsewhere).
    if (shouldEnqueueSend(session.state, connecting)) {
      sendQueue.enqueue({
        storedDisplay,
        attachments: att,
        createGoal: goalModeSelected,
        planMode: planModeSelected,
        ultraMode: ultraModeSelected,
      });
      clearComposerAfterSubmit();
      return;
    }

    clearComposerAfterSubmit();
    await executeSend({
      storedDisplay,
      att,
      createGoal: goalModeSelected,
      planMode: planModeSelected,
      ultraMode: ultraModeSelected,
      targetSessionId: session.sessionId,
    });
  };
  sendRef.current = send;

  const editAndResendLastUserMessage = async (
    message: ChatMessage,
    content: string,
  ): Promise<boolean> => {
    const sessionId = session.sessionId;
    if (!sessionId || session.state === "streaming" || sendInFlightRef.current) {
      return false;
    }
    try {
      const prepared = await sessionPrepareEditLastUser({
        sessionId,
        expectedText: message.content,
      });
      updateSessionPreference(prepared.archivedBranchId, { archived: true });

      // Host 已将原 Session 截断；同步收窄前端可丢弃投影，避免新请求在旧尾部
      // 尚未完成 replay 时短暂携带已废弃的 Assistant 轨迹。
      const view = ensureAcpSession(acpWorkspaceRef.current, sessionId);
      for (let index = view.history.length - 1; index >= 0; index -= 1) {
        if (view.history[index]?.role === "user") {
          view.history.splice(index);
          break;
        }
      }
      view.live_segments = [];
      commitWorkspace();
      patchSessionMessages(sessionId, (current) => {
        let index = -1;
        for (let cursor = current.length - 1; cursor >= 0; cursor -= 1) {
          if (current[cursor]?.role === "user") {
            index = cursor;
            break;
          }
        }
        return index >= 0 ? current.slice(0, index) : current;
      });
      applyViewProjectionRef.current(sessionId);
      await refreshSessions();

      return await executeSend({
        storedDisplay: content,
        att: message.attachments ?? [],
        planMode: planModeSessionKey === sessionId,
        ultraMode: ultraModeSessionKey === sessionId,
        targetSessionId: sessionId,
      });
    } catch (cause) {
      setLocalError(localizeUiError(cause, locale));
      return false;
    }
  };

  executeSendFromQueueRef.current = (opts) => executeSend(opts);

  const queuePreviewLabels = useMemo(
    () => ({
      filesCount: (n: number) =>
        tr("composer.queueFilesCount", { n: String(n) }),
      empty: tr("composer.queueEmptyPreview"),
    }),
    [tr],
  );

  const steerQueuedItem = async (item: QueuedSend) => {
    const sessionId = session.sessionId;
    if (!sessionId || session.state !== "streaming") {
      throw new Error(tr("composer.queueSteerNotRunning"));
    }
    const segments = parseStoredContent(item.storedDisplay);
    const agentBody = serializeForAgent(segments);
    if (item.createGoal) {
      const objective = agentBody.trim();
      if (!objective) throw new Error(tr("goal.objectiveRequired"));
      const result = await goalUpsert({
        sessionId,
        goal: { title: objective, description: objective },
      });
      const view = ensureAcpSession(acpWorkspaceRef.current, sessionId);
      view.goal = { revision: result.revision, goal: result.goal };
      commitWorkspace();
    }
    const text = buildAgentPrompt(agentBody, item.attachments);
    await sessionSteer({ sessionId, text });
    showToast(tr("composer.queueSteered"), 2200);
  };

  const addAttachmentsFromPaths = useCallback(

    async (paths: string[]) => {
      if (!paths.length) {
        setLocalError(tr("attach.droppedNone"));
        return;
      }
      try {
        if (!api.isTauri()) {
          setAttachments((prev) =>
            mergeAttachments(
              prev,
              paths.map((p) => ({
                path: p,
                name: p.split(/[/\\]/).pop() || p,
                isDir: false,
              })),
            ),
          );
          return;
        }
        const classified = await api.pathsClassify(paths);
        // Accept all formats (images, docs, …). Keep entries even if exists is false
        // so transient sandbox / iCloud paths still show; open may fail later.
        const next = classified.map((c) => ({
          path: c.path,
          name: c.name,
          isDir: c.isDir,
        }));
        if (!next.length) {
          setLocalError(tr("attach.droppedNone"));
          return;
        }
        setAttachments((prev) => mergeAttachments(prev, next));
        setLocalError(null);
      } catch (e) {
        setLocalError(localizeUiError(e, locale));
      }
    },
    [tr],
  );

  const closeComposerMenu = useCallback(() => {
    const live = liveSlashRef.current;
    if (live.present) {
      slashDismissedSigRef.current = `${live.start}:${live.query}`;
    }
    setShowComposerPlus(false);
    setSlashQuery(null);
    const cleared = { present: false, query: "", start: 0, end: 0 };
    setLiveSlash(cleared);
    liveSlashRef.current = cleared;
  }, []);

  /** Stable slash-query setter: skip no-op updates so filter effects don't thrash. */
  const onSlashQueryChange = useCallback(
    (q: { start: number; query: string; end: number } | null) => {
      setSlashQuery((prev) => {
        if (q == null) return prev == null ? prev : null;
        if (
          prev &&
          prev.start === q.start &&
          prev.query === q.query &&
          prev.end === q.end
        ) {
          return prev;
        }
        return q;
      });
    },
    [],
  );

  const pickComposerFiles = useCallback(async () => {
    closeComposerMenu();
    if (!api.isTauri()) {
      setLocalError(tr("composer.attachPasteFailed"));
      return;
    }
    try {
      const paths = await api.pickAttachFiles();
      if (!paths.length) {
        // Cancelled — no error.
        return;
      }
      await addAttachmentsFromPaths(paths);
      setLocalError(null);
      const label =
        paths.length === 1
          ? paths[0]!.split(/[/\\]/).pop() || paths[0]!
          : tr("composer.attachCount", { n: String(paths.length) });
      const msg =
        paths.length === 1
          ? tr("composer.attachSaved", { name: label })
          : tr("composer.attachSaved", { name: label });
      setToast(msg);
      window.setTimeout(
        () => setToast((cur) => (cur === msg ? null : cur)),
        2200,
      );
    } catch (e) {
      setLocalError(localizeUiError(e, locale));
    }
  }, [addAttachmentsFromPaths, closeComposerMenu, tr]);

  const addPastedFiles = useCallback(
    async (files: File[]) => {
      if (!files.length || !api.isTauri()) return;
      const claimed = claimClipboardFiles(files, claimedClipboardFilesRef.current);
      if (!claimed.length) return;
      try {
        const paths: string[] = [];
        for (const file of claimed) {
          paths.push(
            await api.savePastedAttachment(
              file.name || "pasted-file",
              Array.from(new Uint8Array(await file.arrayBuffer())),
            ),
          );
        }
        await addAttachmentsFromPaths(paths);
        setLocalError(null);
      } catch (error) {
        setLocalError(localizeUiError(error, locale));
      } finally {
        window.setTimeout(() => claimedClipboardFilesRef.current.clear(), 500);
      }
    },
    [addAttachmentsFromPaths, locale],
  );

  const applyAddProjectSource = useCallback((path: string) => {
    setAddProjectPath(path);
    if (!addProjectNameEditedRef.current) {
      setAddProjectName(
        projects.find((project) => pathsEqual(project.path, path))?.name ??
          pathBasename(path),
      );
    }
    setAddProjectError(null);
  }, [projects]);

  const selectAddProjectSourceFromPaths = useCallback(
    async (paths: string[]) => {
      if (!paths.length || !api.isTauri()) return;
      const request = ++addProjectSourceRequestRef.current;
      // A dropped replacement must not leave the previous folder submittable
      // while the host is still classifying the new path.
      setAddProjectPath("");
      setAddProjectError(null);
      try {
        const classified = await api.pathsClassify(paths);
        if (request !== addProjectSourceRequestRef.current) return;
        const dirs = classified.filter((c) => c.exists && c.isDir);
        if (!dirs.length) {
          setAddProjectError(tr("addProject.folderOnly"));
          return;
        }
        if (dirs.length > 1) {
          setAddProjectError(tr("addProject.oneFolderOnly"));
          return;
        }
        applyAddProjectSource(dirs[0]!.path);
      } catch (e) {
        if (request === addProjectSourceRequestRef.current) {
          setAddProjectError(localizeUiError(e, locale));
        }
      }
    },
    [applyAddProjectSource, locale, tr],
  );

  const hitDragZone = useCallback(
    (clientX: number, clientY: number): DragZone => {
      return hitDragZoneFromRects(
        clientX,
        clientY,
        addProjectDropRef.current?.getBoundingClientRect() ?? null,
        addProjectIntent !== null,
      );
    },
    [addProjectIntent],
  );

  // Tauri OS file drag-drop (full absolute paths)
  useEffect(() => {
    if (!api.isTauri()) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    void (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const webview = getCurrentWebview();
        const win = getCurrentWindow();
        const factor = await win.scaleFactor();

        const stopListening = await webview.onDragDropEvent((event) => {
          if (cancelled) return;
          const payload = event.payload;
          if (payload.type === "enter" || payload.type === "drop") {
            if ("paths" in payload && payload.paths?.length) {
              dragPathsRef.current = payload.paths;
            }
          }
          if (payload.type === "leave") {
            setDragZone(null);
            dragPathsRef.current = [];
            return;
          }
          if (payload.type === "enter" || payload.type === "over") {
            // macOS: coords are already view points; win: physical → / factor
            const { x, y } = toClientDragPoint(
              payload.position,
              factor,
              platform,
            );
            setDragZone(hitDragZone(x, y));
            return;
          }
          if (payload.type === "drop") {
            const { x, y } = toClientDragPoint(
              payload.position,
              factor,
              platform,
            );
            const zone = hitDragZone(x, y);
            const paths = payload.paths?.length
              ? payload.paths
              : dragPathsRef.current;
            setDragZone(null);
            dragPathsRef.current = [];
            if (!paths.length) {
              setLocalError(tr("attach.droppedNone"));
              return;
            }
            if (zone === "project") {
              void selectAddProjectSourceFromPaths(paths);
            } else if (zone === "main") {
              // 主区域接收图片和其他通用文件附件。
              void addAttachmentsFromPaths(paths);
            }
          }
        });
        if (cancelled) stopListening();
        else unlisten = stopListening;
      } catch {
        /* webview API unavailable */
      }
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [
    addAttachmentsFromPaths,
    hitDragZone,
    platform,
    selectAddProjectSourceFromPaths,
    tr,
  ]);

  // HTML5 fallback: some image drags only expose File list in the webview.
  // Prefer Tauri paths; use File.path when present (Tauri webview).
  useEffect(() => {
    const onDragOver = (e: DragEvent) => {
      if (!e.dataTransfer?.types?.includes("Files")) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "copy";
    };
    const onDrop = (e: DragEvent) => {
      if (!e.dataTransfer?.files?.length) return;
      // If Tauri already handled this OS drop, paths may be empty here.
      const files = Array.from(e.dataTransfer.files);
      const paths = files
        .map((f) => {
          const anyF = f as File & { path?: string };
          return anyF.path || "";
        })
        .filter(Boolean);
      const zone = hitDragZone(e.clientX, e.clientY);
      if (paths.length) {
        e.preventDefault();
        e.stopPropagation();
        if (zone === "project") void selectAddProjectSourceFromPaths(paths);
        else if (zone === "main") void addAttachmentsFromPaths(paths);
        return;
      }
    };
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragover", onDragOver);
      window.removeEventListener("drop", onDrop);
    };
  }, [
    addAttachmentsFromPaths,
    hitDragZone,
    selectAddProjectSourceFromPaths,
  ]);

  // Drag-resize left session rail
  useEffect(() => {
    if (!resizingSidebar) return;
    const onMove = (e: PointerEvent) => {
      const collapsed = shouldCollapsePane(e.clientX, SIDEBAR_WIDTH_MIN);
      const next = clampSidebarWidth(e.clientX);
      setLayout((l) => {
        const n = { ...l, sidebarWidth: next, sidebarCollapsed: collapsed };
        if (collapsed) saveLayout(localStorage, n);
        return n;
      });
      if (collapsed) setResizingSidebar(false);
    };
    const onUp = () => {
      setResizingSidebar(false);
      setLayout((l) => {
        saveLayout(localStorage, l);
        return l;
      });
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
  }, [resizingSidebar]);

  // Drag-resize right resource pane
  useEffect(() => {
    if (!resizingAside) return;
    const onMove = (e: PointerEvent) => {
      const rawWidth = window.innerWidth - e.clientX;
      const collapsed = shouldCollapsePane(rawWidth, ASIDE_WIDTH_MIN);
      setLayout((l) => {
        const next = clampAsideWidth(
          rawWidth,
          window.innerWidth - (l.sidebarCollapsed ? 0 : l.sidebarWidth),
        );
        const n = {
          ...l,
          asideWidth: next,
          asideCollapsed: collapsed,
        };
        if (collapsed) saveLayout(localStorage, n);
        return n;
      });
      if (collapsed) setResizingAside(false);
    };
    const onUp = () => {
      setResizingAside(false);
      setLayout((l) => {
        saveLayout(localStorage, l);
        return l;
      });
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
  }, [resizingAside]);

  const resizeComposer = (el: HTMLElement) => {
    const line = 22; // ~line-height
    const min = line * 2;
    const max = line * 10;
    el.style.height = "auto";
    el.style.height = `${Math.min(Math.max(el.scrollHeight, min), max)}px`;
  };

  /** Programmatic draft / layout changes: recompute height after paint. */
  const syncComposerHeight = useCallback(() => {
    // 双 rAF 等待 React 提交和布局完成。
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        const node = composerInputRef.current;
        if (node) resizeComposer(node);
      });
    });
  }, []);

  // 为 Slash 选择器按需加载当前 Skill 目录。
  useEffect(() => {
    if (!api.isTauri()) return;
    let cancelled = false;
    setSkillsLoading(true);
    void api
      .skillsList(activeProject?.path ?? null)
      .then((res) => {
        if (cancelled) return;
        setSkillInfos(
          (res.skills ?? [])
            .map((s) => ({
              name: s.name,
              description: s.description ?? "",
              source: s.source,
              userInvocable: s.userInvocable,
            })),
        );
      })
      .catch(() => {
        if (!cancelled) setSkillInfos([]);
      })
      .finally(() => {
        if (!cancelled) setSkillsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [activeProject?.path]);

  const slashCatalog = useMemo(
    () => buildSlashCatalog(skillInfos),
    [skillInfos],
  );
  const resolveSlashTitle = useCallback(
    (item: SlashItem) => {
      if (item.titleKey) {
        try {
          return tr(item.titleKey as MessageKey);
        } catch {
          /* fall through */
        }
      }
      return item.displayTitle || item.name;
    },
    [tr],
  );
  const resolveSlashDescription = useCallback(
    (item: SlashItem) => {
      if (item.descriptionKey) {
        try {
          return tr(item.descriptionKey as MessageKey);
        } catch {
          /* fall through */
        }
      }
      return item.displayDescription || "";
    },
    [tr],
  );
  /** Filter query from live editor poll only. */
  const slashFilterQuery = liveSlash.present ? liveSlash.query : "";

  /** Shared filter for + menu and `/` slash — empty query = full catalog. */
  const slashFiltered = useMemo(
    () =>
      flattenFilteredCatalog(slashCatalog, slashFilterQuery, (item) => ({
        title: resolveSlashTitle(item),
        description: resolveSlashDescription(item),
      })),
    [
      slashCatalog,
      slashFilterQuery,
      resolveSlashTitle,
      resolveSlashDescription,
    ],
  );
  const showUploadInMenu = useMemo(
    () =>
      uploadMatchesQuery(slashFilterQuery, {
        title: tr("composer.addFiles"),
        hint: tr("composer.addFilesHint"),
      }),
    [slashFilterQuery, tr],
  );
  const composerMenuEntries = useMemo(
    () =>
      buildComposerPlusEntries({
        showUpload: showUploadInMenu,
        commands: slashFiltered.commands,
        skills: slashFiltered.skills,
      }),
    [showUploadInMenu, slashFiltered.commands, slashFiltered.skills],
  );
  const composerMenuEntriesRef = useRef(composerMenuEntries);
  composerMenuEntriesRef.current = composerMenuEntries;

  /** + button and `/` open the same panel. */
  const composerMenuOpen = showComposerPlus || liveSlash.present;

  /**
   * rAF poll of composer innerText → live slash token.
   * Single source of truth for open state + filter (not React draft).
   */
  useEffect(() => {
    let raf = 0;
    let alive = true;
    const tick = () => {
      if (!alive) return;
      const el = composerInputRef.current;
      const detected = detectSlashQueryFromEditor(el);
      let next = detected
        ? {
            present: true as const,
            query: detected.query,
            start: detected.start,
            end: detected.end,
          }
        : {
            present: false as const,
            query: "",
            start: 0,
            end: 0,
          };
      // Honor Escape dismiss until the user edits the `/token`.
      if (next.present && slashDismissedSigRef.current != null) {
        const sig = `${next.start}:${next.query}`;
        if (sig === slashDismissedSigRef.current) {
          next = { present: false, query: "", start: 0, end: 0 };
        } else {
          slashDismissedSigRef.current = null;
        }
      }
      if (!next.present && detected == null) {
        slashDismissedSigRef.current = null;
      }
      const prev = liveSlashRef.current;
      if (
        prev.present !== next.present ||
        prev.query !== next.query ||
        prev.start !== next.start ||
        prev.end !== next.end
      ) {
        liveSlashRef.current = next;
        setLiveSlash(next);
        if (next.present) {
          setSlashQuery({
            start: next.start,
            query: next.query,
            end: next.end,
          });
        } else if (!showComposerPlusRef.current) {
          setSlashQuery((q) => (q == null ? q : null));
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => {
      alive = false;
      cancelAnimationFrame(raf);
    };
  }, []);

  /** Pin above input card; width matches composer shell.
   * Re-anchor when filter results change height (short list must sit on input). */
  const { pos: composerPlusPos, style: composerPlusStyle } = useFloatingMenu({
    open: composerMenuOpen,
    triggerRef: composerShellRef,
    panelRef: composerPlusPanelRef,
    roots: [composerPlusTriggerRef, composerShellRef, composerInputRef],
    onClose: closeComposerMenu,
    placement: "up",
    fitContent: false,
    matchTriggerWidth: true,
    minWidth: 280,
    estHeight: 220,
    gap: 8,
    deps: [slashFilterQuery, composerMenuEntries.length],
  });

  const sessionPromptHistory = useMemo(
    () => collectUserPromptHistory(messages),
    [messages],
  );
  const promptHistoryEntries = useMemo(
    () => filterPromptHistory(sessionPromptHistory, promptHistoryFilter),
    [sessionPromptHistory, promptHistoryFilter],
  );

  const closePromptHistory = useCallback(() => {
    setPromptHistoryOpen(false);
    setPromptHistoryFilter("");
    setPromptHistoryActive(0);
    setPromptHistoryFocusFilter(false);
  }, []);

  const applyPromptHistoryEntry = useCallback(
    (
      entry: PromptHistoryEntry,
      opts?: { close?: boolean; listIndex?: number },
    ) => {
      promptHistoryIndexRef.current = entry.historyIndex;
      setPromptHistoryIndex(entry.historyIndex);
      if (typeof opts?.listIndex === "number") {
        setPromptHistoryActive(opts.listIndex);
      }
      setDraft(entry.text);
      if (opts?.close !== false) {
        closePromptHistory();
        requestAnimationFrame(() => {
          composerInputRef.current?.focus?.();
        });
      }
    },
    [closePromptHistory],
  );

  const { pos: promptHistoryPos, style: promptHistoryStyle } = useFloatingMenu({
    open: promptHistoryOpen,
    triggerRef: composerShellRef,
    panelRef: promptHistoryPanelRef,
    roots: [composerShellRef, composerInputRef, promptHistoryPanelRef],
    onClose: closePromptHistory,
    placement: "up",
    fitContent: false,
    matchTriggerWidth: true,
    minWidth: 280,
    estHeight: 280,
    gap: 8,
    deps: [promptHistoryFilter, promptHistoryEntries.length],
  });

  // Keep highlight in range when the filtered list shrinks; reset on filter text.
  const prevPromptHistoryFilterRef = useRef(promptHistoryFilter);
  useEffect(() => {
    if (!promptHistoryOpen) return;
    if (prevPromptHistoryFilterRef.current !== promptHistoryFilter) {
      prevPromptHistoryFilterRef.current = promptHistoryFilter;
      setPromptHistoryActive(0);
      return;
    }
    setPromptHistoryActive((i) => {
      if (promptHistoryEntries.length === 0) return 0;
      return i >= promptHistoryEntries.length
        ? promptHistoryEntries.length - 1
        : i;
    });
  }, [promptHistoryEntries.length, promptHistoryFilter, promptHistoryOpen]);

  // Reset highlight only when the filter *string* changes.
  const prevFilterQueryRef = useRef(slashFilterQuery);
  useEffect(() => {
    if (prevFilterQueryRef.current === slashFilterQuery) return;
    prevFilterQueryRef.current = slashFilterQuery;
    setSlashActiveIndex(0);
  }, [slashFilterQuery]);

  // Keep highlight in range when the filtered list shrinks (no forced 0).
  useEffect(() => {
    setSlashActiveIndex((i) => {
      if (composerMenuEntries.length === 0) return 0;
      return i >= composerMenuEntries.length
        ? composerMenuEntries.length - 1
        : i;
    });
  }, [composerMenuEntries.length]);

  const showToast = useCallback((msg: string, ms = 3200) => {
    setToast(msg);
    window.setTimeout(() => {
      setToast((cur) => (cur === msg ? null : cur));
    }, ms);
  }, []);

  /**
   * Open current-session prompt history picker (Build `/history`).
   * @param focusFilter — true for slash `/history` (search box); false for empty ↑.
   * @param seedDraft — fill composer with the active row (empty ↑).
   */
  const openPromptHistory = useCallback(
    (opts?: { focusFilter?: boolean; seedDraft?: boolean }) => {
      const history = collectUserPromptHistory(messagesRef.current);
      if (history.length === 0) {
        showToast(tr("slash.historyEmpty"), 2400);
        return;
      }
      // Don't stack with slash/plus menu.
      setShowComposerPlus(false);
      setSlashQuery(null);
      setLiveSlash({ present: false, query: "", start: 0, end: 0 });
      liveSlashRef.current = { present: false, query: "", start: 0, end: 0 };

      setPromptHistoryFilter("");
      setPromptHistoryActive(0);
      setPromptHistoryFocusFilter(opts?.focusFilter === true);
      setPromptHistoryOpen(true);
      if (opts?.seedDraft !== false) {
        promptHistoryIndexRef.current = 0;
        setPromptHistoryIndex(0);
        setDraft(history[0] ?? "");
      }
    },
    [showToast, tr],
  );


  const sendQueueLabels = useMemo(
    () => ({
      queued: tr("composer.queued"),
      sendFailed: tr("composer.queueSendFailed"),
      droppedOldest: (n: number, max: number) =>
        tr("composer.queueDroppedOldest", {
          n: String(n),
          max: String(max),
        }),
    }),
    [tr],
  );
  const sendQueue = useSendQueue({
    sessionId: session.sessionId,
    sessionState: session.state,
    connecting,
    liveHostRef,
    viewingSessionIdRef,
    sendInFlightRef,
    executeSendRef: executeSendFromQueueRef,
    showToast,
    labels: sendQueueLabels,
  });

  /** 分叉完整 Session 并打开新任务。 */
  const runForkSession = useCallback(
    async (source: SessionRow) => {
      if (!api.isTauri()) {
        showToast(tr("error.needTauri"));
        return;
      }
      try {
        const base = (source.title || tr("session.untitled")).trim();
        // Avoid double-prefix when forking a fork (any locale).
        const title = /^(fork of|分叉：|分叉:)\s*/i.test(base)
          ? base
          : tr("session.forkTitleOf", { name: base || "chat" });
        const meta = await sessionFork({ sourceId: source.id, title });
        await refreshSessions();
        const row: SessionRow = {
          id: meta.id,
          title,
          projectId: source.projectId,
          updatedAt: new Date().toISOString(),
          archived: false,
          pinned: false,
        };
        const proj = row.projectId
          ? projects.find((p) => p.id === row.projectId) ?? null
          : null;
        if (row.projectId) {
          setExpandedProjects((e) => ({ ...e, [row.projectId!]: true }));
        } else {
          setHistoryOpen(true);
        }
        await openSession(row, proj);
        showToast(tr("session.forkOk"), 2800);
      } catch (e) {
        showToast(tr("session.forkFailed") + ": " + String(e), 4500);
      }
    },
    // openSession / refreshSessions via closure
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [projects, showToast, tr],
  );

  const confirmForkSession = useCallback(
    (source: SessionRow) => {
      setCtxMenu(null);
      setAppDialog({
        kind: "confirm",
        title: tr("session.forkTitle"),
        message: tr("session.forkConfirm"),
        confirmLabel: tr("session.fork"),
        onConfirm: () => {
          void runForkSession(source);
        },
      });
    },
    [runForkSession, tr],
  );

  const applySlashItem = useCallback(
    (item: SlashItem) => {
      const live = liveSlashRef.current;
      const q =
        slashQuery ??
        (live.present
          ? { start: live.start, query: live.query, end: live.end }
          : null);
      setSlashQuery(null);
      setLiveSlash({ present: false, query: "", start: 0, end: 0 });
      liveSlashRef.current = { present: false, query: "", start: 0, end: 0 };
      setShowComposerPlus(false);

      if (item.kind === "skill") {
        if (q) {
          setDraft((d) => applySkillAtSlash(d, q.start, q.end, item.name));
        } else {
          setDraft((d) => {
            const needsSpace = d.length > 0 && !/\s$/.test(d);
            return `${d}${needsSpace ? " " : ""}[[skill:${item.name}]] `;
          });
        }
        return;
      }

      // Remove the /query from draft for the selected action.
      if (q) {
        setDraft((d) => d.slice(0, q.start) + d.slice(q.end));
      }

      if (item.kind === "action") {
        switch (item.action) {
          case "goal": {
            if (!acpSessionView?.goal.goal) {
              setGoalModeSessionKey(session.sessionId ?? "__draft__");
              // 目标模式与计划模式互斥：开启目标模式时退出计划模式。
              setPlanModeSessionKey(null);
            }
            return;
          }
          case "plan": {
            const key = session.sessionId ?? "__draft__";
            // slash 处理器为宽依赖 useCallback，用函数式更新避免读到过期开关值。
            setPlanModeSessionKey((prev) => (prev === key ? null : key));
            // 目标模式与计划模式互斥：开启计划模式时退出目标模式。
            // 关闭时目标模式必然为空（互斥不变量），这里置空无副作用。
            setGoalModeSessionKey(null);
            return;
          }
          case "status":
            setShowStatusModal(true);
            return;
          case "newChat":
            void newChat();
            return;
          case "export":
            void exportActiveSessionMd();
            return;
          default:
            return;
        }
      }
    },
    // many deps — intentionally broad for stable handlers used in render
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [slashQuery],
  );

  // 草稿或任务切换后重新计算输入框高度，并补偿尚未完成的聚焦请求。
  useEffect(() => {
    if (pendingComposerFocus.current) {
      requestComposerFocus();
      return;
    }
    syncComposerHeight();
  }, [draft, session.sessionId, requestComposerFocus, syncComposerHeight]);

  /** Context usage chip label/state from the latest ACP usage event. */
  const contextUsageDisplay = useMemo(() => {
    const source = contextUsage?.estimated ? "estimated" : "known";
    const display = contextUsage
      ? {
          tokens: contextUsage.used,
          source: source as "known" | "estimated",
          label: formatContextChipLabel(contextUsage.used, source),
        }
      : { tokens: null, source: "unknown" as const, label: "—" };
    const catalogContextWindow = modelMetadataById[modelId]?.contextWindow;
    return attachContextWindow(display, contextUsage?.size ?? catalogContextWindow);
  }, [contextUsage, modelId, modelMetadataById]);

  /** Goal 工具完成签名，用于完成后立即刷新输入框上方的目标投影。 */
  const goalToolCompletionSignature = useMemo(() => {
    if (acpSessionView?.session_id !== session.sessionId) return "";
    return acpSessionView.live_segments
      .filter(
        (segment) =>
          segment.kind === "tool" &&
          isGoalToolName(segment.toolKind, segment.title) &&
          !segment.streaming &&
          segment.status === "completed",
      )
      .map((segment) =>
        segment.kind === "tool" ? segment.toolCallId : "",
      )
      .join("|");
  }, [acpSessionView, session.sessionId]);

  /** 切换 Session 或 Goal 工具完成时刷新 Goal，不启动后台轮询。 */
  useEffect(() => {
    const sessionId = session.sessionId;
    if (!api.isTauri() || !sessionId) return;
    void goalsList(sessionId)
      .then((result) => {
        const view = acpWorkspaceRef.current.sessions[sessionId];
        if (!view) return;
        reduceGoalSnapshot(view, result.revision, result.goals);
        commitWorkspace();
        applyViewProjectionRef.current(sessionId);
      })
      .catch(() => {});
  }, [
    commitWorkspace,
    goalToolCompletionSignature,
    session.sessionId,
  ]);

  /** 请求清除当前 Session 的持久目标，并在确认后同步本地投影。 */
  const confirmClearCurrentGoal = useCallback(() => {
    const sessionId = session.sessionId;
    const currentGoal = acpSessionView?.goal.goal;
    if (!sessionId || !currentGoal) return;
    setAppDialog({
      kind: "confirm",
      title: tr("goal.clearTitle"),
      message: tr("goal.clearConfirm", { title: currentGoal.title }),
      confirmLabel: tr("goal.clear"),
      danger: true,
      onConfirm: async () => {
        try {
          await goalClear(sessionId);
          const view = acpWorkspaceRef.current.sessions[sessionId];
          if (view) {
            view.goal = { revision: 0, goal: null };
            commitWorkspace();
          }
        } catch (cause) {
          showToast(tr("goal.clearFailed", { error: String(cause) }), 4000);
        }
      },
    });
  }, [acpSessionView?.goal.goal, commitWorkspace, session.sessionId, showToast, tr]);

  /** 打开当前 Goal 的单字段编辑弹窗，并同步保存后的权威投影。 */
  const editCurrentGoal = useCallback(() => {
    const sessionId = session.sessionId;
    const projection = acpSessionView?.goal;
    const currentGoal = projection?.goal;
    if (!sessionId || !projection || !currentGoal) return;
    setAppDialog({
      kind: "prompt",
      title: tr("goal.editTitle"),
      initial: currentGoal.objective || currentGoal.title,
      placeholder: tr("goal.editPlaceholder"),
      onSubmit: async (value) => {
        const title = value.trim();
        if (!title || title === currentGoal.objective) return;
        try {
          const result = await goalUpsert({
            sessionId,
            goal: { title, description: title },
            expectedRevision: projection.revision,
            requestNonce: `keencode-${Date.now()}-${Math.random().toString(36).slice(2)}`,
          });
          const view = acpWorkspaceRef.current.sessions[sessionId];
          if (view) {
            view.goal = { revision: result.revision, goal: result.goal };
            commitWorkspace();
          }
        } catch (cause) {
          showToast(tr("goal.editFailed", { error: String(cause) }), 4000);
        }
      },
    });
  }, [acpSessionView?.goal, commitWorkspace, session.sessionId, showToast, tr]);

  /**
   * In-chat find matches — user + assistant bodies only.
   * Historical tool_step rows are not rendered in the transcript, so matching
   * them would land on invisible hits.
   */
  const chatFindMatches = useMemo((): ChatFindMatch[] => {
    if (!showChatFind) return [];
    return findChatMatches(
      chatFindQuery,
      messages
        .filter((m) => m.role === "user" || m.role === "assistant")
        .map((m) => ({
          id: m.id,
          role: m.role,
          content: m.content,
          marker: m.marker,
        })),
    );
  }, [showChatFind, chatFindQuery, messages]);

  const chatFindHitIds = useMemo(() => {
    const s = new Set<string>();
    for (const m of chatFindMatches) s.add(m.messageId);
    return s;
  }, [chatFindMatches]);

  const chatFindActive = useMemo(() => {
    if (!showChatFind || chatFindMatches.length === 0) return null;
    const idx =
      chatFindIndex >= 0 && chatFindIndex < chatFindMatches.length
        ? chatFindIndex
        : 0;
    const hit = chatFindMatches[idx]!;
    return { messageId: hit.messageId, occurrence: hit.occurrence };
  }, [showChatFind, chatFindMatches, chatFindIndex]);

  // Clamp active index when the match list shrinks (query edit / new messages).
  useEffect(() => {
    if (!showChatFind) return;
    if (chatFindMatches.length === 0) {
      if (chatFindIndex !== 0) setChatFindIndex(0);
      return;
    }
    if (chatFindIndex >= chatFindMatches.length) {
      setChatFindIndex(0);
    }
  }, [showChatFind, chatFindMatches.length, chatFindIndex]);

  // Reset find when switching conversation (keep open across same session).
  useEffect(() => {
    setShowChatFind(false);
    setChatFindQuery("");
    setChatFindIndex(0);
  }, [session.sessionId]);

  useEffect(() => {
    if (!showChatFind) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (e.isComposing) return;
      // 弹窗打开时由弹窗处理 Escape。
      if (appDialog) return;
      e.preventDefault();
      e.stopPropagation();
      setShowChatFind(false);
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [showChatFind, appDialog]);

  const [chatFindFocusKey, setChatFindFocusKey] = useState(0);
  const openChatFind = useCallback(() => {
    setShowChatFind(true);
    setChatFindFocusKey((k) => k + 1);
  }, []);

  const chatFindNext = useCallback(() => {
    setChatFindIndex((i) =>
      stepChatFindIndex(i, chatFindMatches.length, 1),
    );
  }, [chatFindMatches.length]);

  const chatFindPrev = useCallback(() => {
    setChatFindIndex((i) =>
      stepChatFindIndex(i, chatFindMatches.length, -1),
    );
  }, [chatFindMatches.length]);

  /** 仅在全新草稿中居中显示空态引导和输入框。 */
  const welcomeSession =
    !session.sessionId &&
    messages.length === 0 &&
    session.state !== "streaming";
  const showWelcomeCopy =
    welcomeSession &&
    isDraftEmpty(parseStoredContent(draft)) &&
    attachments.length === 0;
  const emptyExistingSession =
    !!session.sessionId &&
    messages.length === 0 &&
    session.state !== "streaming" &&
    session.state !== "connecting";
  /** 至少发出过一条用户消息后才展示上下文占用。 */
  const hasStartedConversation = messages.some(
    (message) => message.role === "user",
  );
  /** 当前激活的自定义模型供应商。 */
  const [activeCustomProvider, setActiveCustomProvider] =
    useState<api.CustomProvider | null>(null);
  /** 当前实际交给 peri 的自定义模型标识。 */
  const [activeCustomModelId, setActiveCustomModelId] = useState<string | null>(
    null,
  );
  /** 供应商配置保存版本；即使 ID/模型不变，也要让当前会话重新建缓存。 */
  const [providerRouteRevision, setProviderRouteRevision] = useState(0);
  const providerRouteReadyRef = useRef(false);
  const refreshProviderRoute = useCallback(async () => {
    if (!api.isTauri()) {
      setActiveCustomProvider(null);
      setActiveCustomModelId(null);
      setConfiguredModels([]);
      return;
    }
    try {
      const list = await api.providersList();
      const active =
        list.providers.find((provider) => provider.id === list.activeProviderId) ??
        null;
      setActiveCustomProvider(active);
      setActiveCustomModelId(list.defaultModel);
      const providerModels = list.providers
        .flatMap<ModelOption>((provider) =>
          provider.models.map((model) => ({
            providerId: provider.id,
            providerLabel: provider.name.trim() || provider.id,
            id: model,
            label: model,
            isDefault:
              list.activeProviderId === provider.id &&
              list.defaultModel === model,
            source: provider.apiBackend,
            // 上下文窗口随模型进入菜单：1M 标志（最高优先级）→ 手工配置 → 元数据目录。
            contextWindow: provider.context1m?.[model]
              ? 1_000_000
              : provider.contextWindows?.[model],
          })),
        );
      setConfiguredModels(providerModels);
      const defaultModel = pickNewChatModel(
        list.activeProviderId,
        list.defaultModel,
        providerModels,
      );
      setModelId(defaultModel?.id ?? "");
    } catch {
      /* keep previous */
    }
  }, []);
  /** 仅按 modelId 查询固定公共目录，供应商名称和地址不参与匹配。 */
  useEffect(() => {
    if (!api.isTauri() || !modelId) return;
    let cancelled = false;
    void api
      .modelMetadataGet(modelId)
      .then((metadata) => {
        if (cancelled || metadata.modelId !== modelId) return;
        setModelMetadataById((current) => {
          if (current[modelId]?.updatedAt === metadata.updatedAt) return current;
          return { ...current, [modelId]: metadata };
        });
        const model = applyModelMetadata(
          { id: modelId, label: modelId },
          metadata,
        );
        const efforts = effortsForModel(model);
        if (efforts.length > 0) {
          setEffort((current) =>
            efforts.some((entry) => entry.id === current)
              ? current
              : pickDefaultEffort(model),
          );
        }
        if (
          !viewingSessionIdRef.current &&
          activeCustomProvider?.id &&
          activeCustomModelId === modelId
        ) {
          void api
            .providersSelectModel(activeCustomProvider.id, modelId)
            .catch((error) =>
              diagnosticsRecord(
                "frontend.model_context_window_reload",
                `${modelId}: ${String(error)}`,
              ),
            );
        }
      })
      .catch((error) => {
        void diagnosticsRecord(
          "frontend.model_metadata",
          `${modelId}: ${String(error)}`,
        ).catch(() => {});
      });
    return () => {
      cancelled = true;
    };
  }, [activeCustomModelId, activeCustomProvider?.id, modelId]);
  /** 只有活动供应商和活动模型都仍有效时，才允许提交或排队消息。 */
  const hasConfiguredModel = hasConfiguredProviderModel(
    activeCustomProvider?.id,
    activeCustomModelId,
    availableModels,
  );
  useEffect(() => {
    void refreshProviderRoute();
  }, [refreshProviderRoute]);
  useEffect(() => {
    // 首次启动时供应商目录仍在异步恢复，不能为了切换本地路由调用
    // session_disconnect，否则会干扰仍在执行的会话。
    if (appBooting) return;
    if (!providerRouteReadyRef.current) {
      providerRouteReadyRef.current = true;
      return;
    }
    // 供应商配置保存（可能增删改，旧会话的 provider 引用可能失效）后断开
    // 当前会话回到新会话视图；模型切换不再触发重置——每会话独立 provider
    // （Q1 决策），会话内切模型走 session_set_model，不碰全局路由。
    void (async () => {
      try {
        await sessionDisconnect();
      } catch {
        /* ignore */
      }
      acpWorkspaceRef.current = createAcpWorkspaceState();
      setAcpWorkspace(createAcpWorkspaceState());
      activeTurnIdBySessionRef.current.clear();
      recoverableCompletedTurnIdBySessionRef.current.clear();
      completedTurnIdBySessionRef.current.clear();
      turnLatencyBySessionRef.current.clear();
      pendingVisibleTurnBySessionRef.current.clear();
      setSession({ ...IDLE_SNAPSHOT, state: "idle" });
      setLiveHost({ ...IDLE_SNAPSHOT });
      liveHostRef.current = { ...IDLE_SNAPSHOT };
      viewingSessionIdRef.current = null;
      openingSessionIdRef.current = null;
      openingSessionEpochRef.current = null;
      contextUsageBySessionRef.current.clear();
      setContextUsage(null);
      pendingAskUserBySessionRef.current.clear();
      setPendingAskUserSessionIds(new Set());
      setAskUser(null);
      setMessages([]);
      setRetryStatus(null);
      setLocalError(null);
    })();
  }, [appBooting, providerRouteRevision]);
  // Floating composer height → chat bottom pad so messages can scroll under it.
  useEffect(() => {
    const el = composerWrapRef.current;
    if (!el) return;
    const measure = () => {
      const h = Math.ceil(Math.max(
        el.getBoundingClientRect().height,
        askUserWrapRef.current?.getBoundingClientRect().height ?? 0,
      ));
      if (h <= 0) return;
      // Ignore 1px subpixel flicker — pad thrash reflows chat scrollHeight
      // and looks like the transcript bouncing while you type/scroll.
      setComposerFloatPad((prev) => (Math.abs(prev - h) <= 1 ? prev : h));
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    if (askUserWrapRef.current) ro.observe(askUserWrapRef.current);
    return () => ro.disconnect();
  }, [
    attachments.length,
    draft,
    showComposerPlus,
    messages.length,
    welcomeSession,
    askUser?.rpcId,
  ]);

  const stop = async () => {
    const now = Date.now();
    // Stop belongs to the chat on screen. Preferring the live slot cancelled a
    // foreign turn whenever the viewed chat had been demoted to background.
    const sid =
      viewingSessionIdRef.current || liveHostRef.current.sessionId || null;
    /** 同步清除目标 Session 的持久重试投影，避免后续事件恢复旧提示。 */
    const clearStoppedSessionRetry = () => {
      if (sid) {
        const view = acpWorkspaceRef.current.sessions[sid];
        if (view) view.retry = null;
      }
      setRetryStatus(null);
    };
    const armed = armStopLatch(stopLatchRef.current, sid, now);
    stopLatchRef.current = armed;
    setStopLatch(armed);
    // Force-unlock if Host stays busy past STOP_LATCH_MS.
    window.setTimeout(() => {
      const tick = tickStopLatch(
        stopLatchRef.current,
        liveHostRef.current.state,
        Date.now(),
        STOP_LATCH_MS,
      );
      stopLatchRef.current = tick.latch;
      setStopLatch(tick.latch);
      if (tick.forceComplete) {
        const id = sid || liveHostRef.current.sessionId;
        if (id) {
          patchSessionMessages(id, (prev) =>
            applyTurnMarker(prev, {
              sessionId: id,
              messageId: `end-stop-${Date.now()}`,
              marker: "turn_end",
              reason: "user_stop",
              content: endOfTurnMarkerContent("user_stop"),
            }),
          );
          patchSessionMessages(id, (m) =>
            m.map((x) => ({ ...x, streaming: false })),
          );
        }
        clearStoppedSessionRetry();
        setStreamStall(null);
        setTurnStartedAt(null);
      }
    }, STOP_LATCH_MS + 50);
    try {
      const activeRequestId = sid
        ? activeTurnIdBySessionRef.current.get(sid)
        : null;
      if (sid && !activeRequestId) {
        throw new Error("当前运行回合缺少 requestId，无法安全停止");
      }
      if (sid && activeRequestId) {
        await sessionStop(sid, activeRequestId);
      }
      clearStoppedSessionRetry();
      setStreamStall(null);
      setTurnStartedAt(null);
      const liveId = sid || liveHostRef.current.sessionId;
      if (liveId) {
        patchSessionMessages(liveId, (m) =>
          m.map((x) => ({ ...x, streaming: false })),
        );
        // Prefer a clean end marker when stop settles normally.
        if (stopLatchRef.current.phase !== "force_idle") {
          patchSessionMessages(liveId, (prev) => {
            if (
              prev.some(
                (x) =>
                  x.marker === "turn_end" ||
                  x.marker === "turn_cancelled" ||
                  x.content?.startsWith("turn_end|"),
              )
            ) {
              return prev;
            }
            return applyTurnMarker(prev, {
              sessionId: liveId,
              messageId: `end-stop-ok-${Date.now()}`,
              marker: "turn_end",
              reason: "user_stop",
              content: endOfTurnMarkerContent("user_stop"),
            });
          });
        }
      } else {
        setMessages((m) => m.map((x) => ({ ...x, streaming: false })));
      }
      const cleared = createStopLatchState();
      stopLatchRef.current = cleared;
      setStopLatch(cleared);
    } catch (e) {
      setLocalError(localizeUiError(e, locale));
    } finally {
      if (sid) {
        clearPendingAskUser(sid);
        setAskUser((current) =>
          current?.sessionId === sid ? null : current,
        );
      }
    }
  };

  /**
   * 切换工作目录。peri Session 的工作目录不可变，已有会话时进入目标项目的新草稿。
   */
  const bindSessionProject = useCallback(
    async (proj: Project | null, opts?: { silent?: boolean }) => {
      const sid = session.sessionId;
      if (!sid) {
        setActiveProject(proj);
        if (proj) {
          setExpandedProjects((e) => ({ ...e, [proj.id]: true }));
        } else {
          setHistoryOpen(true);
        }
        return;
      }
      if (proj && isProjectPathMissing(proj.pathOk)) {
        setLocalError(tr("project.pathMissing", { name: proj.name }));
        return;
      }
      try {
        await newChat(proj);
        if (proj) {
          setExpandedProjects((e) => ({ ...e, [proj.id]: true }));
          if (!opts?.silent) {
            showToast(tr("composer.projectBound", { name: proj.name }), 2500);
          }
        } else {
          setHistoryOpen(true);
          if (!opts?.silent) {
            showToast(tr("composer.projectCleared"), 2200);
          }
        }
        setLocalError(null);
      } catch (e) {
        showToast(localizeUiError(e, locale), 4500);
      }
    },
    [newChat, session.sessionId, showToast, tr],
  );

  const gitWorktreesReqRef = useRef(0);
  const gitWorktreesPathRef = useRef<string | null>(null);
  const refreshGitWorktrees = useCallback(async () => {
    const path = activeProject?.path?.trim() || null;
    if (!path || !api.isTauri()) {
      gitWorktreesReqRef.current += 1;
      gitWorktreesPathRef.current = null;
      setGitWorktrees([]);
      setGitWorktreesAvailable(null);
      setGitWorktreesReason(null);
      setGitWorktreesLoading(false);
      return;
    }
    const reqId = ++gitWorktreesReqRef.current;
    // Drop stale rows when the active project path changes; soft-refresh keeps
    // the previous list for the same path so the menu does not flash empty.
    if (gitWorktreesPathRef.current !== path) {
      gitWorktreesPathRef.current = path;
      setGitWorktrees([]);
      setGitWorktreesAvailable(null);
      setGitWorktreesReason(null);
    }
    setGitWorktreesLoading(true);
    try {
      const res = await api.gitWorktreesList(path);
      if (reqId !== gitWorktreesReqRef.current) return;
      if (!res.available) {
        setGitWorktrees([]);
        setGitWorktreesAvailable(false);
        setGitWorktreesReason(res.reason?.trim() || "unavailable");
      } else {
        setGitWorktrees(res.worktrees ?? []);
        setGitWorktreesAvailable(true);
        setGitWorktreesReason(null);
      }
    } catch (e) {
      if (reqId !== gitWorktreesReqRef.current) return;
      setGitWorktrees([]);
      setGitWorktreesAvailable(false);
      setGitWorktreesReason(String(e));
    } finally {
      if (reqId === gitWorktreesReqRef.current) {
        setGitWorktreesLoading(false);
      }
    }
  }, [activeProject?.path]);

  useEffect(() => {
    void refreshGitWorktrees();
  }, [refreshGitWorktrees]);

  /** 添加项目后刷新列表，并按调用场景选中项目或绑定当前任务。 */
  const finalizeAddedProject = useCallback(
    async (p: Project, opts: { bindSession: boolean }) => {
      const list = (await api.projectsList()) as Project[];
      setProjects(list);
      const current = list.find((project) => project.id === p.id) ?? p;
      if (opts.bindSession) {
        await bindSessionProject(current);
      } else {
        setActiveProject(current);
        setExpandedProjects((expanded) => ({
          ...expanded,
          [current.id]: true,
        }));
        showToast(tr("composer.projectAdded", { name: current.name }), 2500);
      }
    },
    [bindSessionProject, showToast, tr],
  );

  /** Open gc dialog and run dry-run preview. */
  const openWorktreeGc = useCallback(() => {
    setWorktreeGcForce(false);
    setWorktreeGcError(null);
    setWorktreeGcBusy(false);
    setWorktreeGcPreview(null);
    setWorktreeGcOpen(true);
  }, []);

  /** Dry-run `git worktree prune` for the modal preview. */
  const refreshWorktreeGcPreview = useCallback(async () => {
    if (!api.isTauri() || !activeProject?.path || !worktreeGcOpen) return;
    setWorktreeGcPreviewBusy(true);
    setWorktreeGcError(null);
    try {
      const res = await api.gitWorktreeGc({
        projectPath: activeProject.path,
        dryRun: true,
        force: worktreeGcForce,
      });
      setWorktreeGcPreview(res);
    } catch (e) {
      setWorktreeGcPreview(null);
      setWorktreeGcError(localizeUiError(e, locale));
    } finally {
      setWorktreeGcPreviewBusy(false);
    }
  }, [activeProject?.path, worktreeGcForce, worktreeGcOpen]);

  useEffect(() => {
    if (!worktreeGcOpen) return;
    void refreshWorktreeGcPreview();
  }, [worktreeGcOpen, refreshWorktreeGcPreview]);

  /** Apply prune (non-dry-run), refresh list, toast. */
  const submitWorktreeGc = useCallback(async () => {
    if (!api.isTauri() || !activeProject?.path) return;
    setWorktreeGcBusy(true);
    setWorktreeGcError(null);
    try {
      const res = await api.gitWorktreeGc({
        projectPath: activeProject.path,
        dryRun: false,
        force: worktreeGcForce,
      });
      setWorktreeGcOpen(false);
      setWorktreeGcPreview(null);
      setWorktreeGcForce(false);
      await refreshGitWorktrees();
      const n = res.prunedCount;
      showToast(
        n > 0
          ? tr("composer.worktreeGcDone", { n: String(n) })
          : tr("composer.worktreeGcDoneNone"),
        2800,
      );
    } catch (e) {
      setWorktreeGcError(localizeUiError(e, locale));
    } finally {
      setWorktreeGcBusy(false);
    }
  }, [
    activeProject?.path,
    refreshGitWorktrees,
    showToast,
    tr,
    worktreeGcForce,
  ]);

  /** Open a linked worktree as project cwd (reuse existing project if path matches). */
  const switchToWorktree = useCallback(
    async (wt: api.GitWorktreeEntry) => {
      if (!api.isTauri()) return;
      const path = wt.path?.trim();
      if (!path) return;
      try {
        const existing = projects.find((p) => pathsEqual(p.path, path));
        if (existing) {
          await bindSessionProject(existing, { silent: true });
          showToast(
            tr("composer.worktreeSwitched", {
              name: existing.name,
              branch: wt.branch || tr("composer.worktreeDetached"),
            }),
            2500,
          );
          return;
        }
        const added = (await api.projectCreate(pathBasename(path), path)) as Project;
        const list = (await api.projectsList()) as Project[];
        setProjects(list);
        const proj = list.find((p) => p.id === added.id) ?? added;
        await bindSessionProject(proj, { silent: true });
        showToast(
          tr("composer.worktreeSwitched", {
            name: proj.name,
            branch: wt.branch || tr("composer.worktreeDetached"),
          }),
          2500,
        );
      } catch (e) {
        showToast(localizeUiError(e, locale), 4500);
      }
    },
    [
      bindSessionProject,
      projects,
      showToast,
      tr,
    ],
  );

  const openWorktreeCreate = useCallback((opts?: { startNewChat?: boolean }) => {
    setWorktreeCreateName("");
    setWorktreeCreateRef("");
    setWorktreeCreateError(null);
    setWorktreeCreateBusy(false);
    setWorktreeCreateStartChat(!!opts?.startNewChat);
    setWorktreeCreateOpen(true);
  }, []);

  const worktreeCreatePreviewPath = (() => {
    try {
      const main = mainWorktreePath(gitWorktrees) || activeProject?.path || "";
      if (!main || !worktreeCreateName.trim()) return null;
      return buildWorktreeSiblingPath(main, worktreeCreateName.trim());
    } catch {
      return null;
    }
  })();

  /**
   * 创建 worktree → 刷新列表 → 加入项目 →
   * 绑定当前 Session，或在该路径创建新草稿。
   */
  const submitWorktreeCreate = useCallback(async () => {
    if (!api.isTauri() || !activeProject?.path) return;
    const rawName = worktreeCreateName.trim();
    if (!rawName) {
      setWorktreeCreateError(tr("composer.worktreeNameRequired"));
      return;
    }
    let safeName: string;
    try {
      safeName = sanitizeWorktreeName(rawName);
    } catch {
      setWorktreeCreateError(tr("composer.worktreeNameInvalid"));
      return;
    }
    setWorktreeCreateBusy(true);
    setWorktreeCreateError(null);
    try {
      const start = worktreeCreateRef.trim() || null;
      const created = await api.gitWorktreeAdd(
        activeProject.path,
        safeName,
        start,
      );
      setWorktreeCreateOpen(false);
      await refreshGitWorktrees();

      const path = created.path;
      const branch =
        created.branch?.trim() ||
        created.name ||
        tr("composer.worktreeDetached");
      const startChat = worktreeCreateStartChat;
      const existing = projects.find((p) => pathsEqual(p.path, path));
      let target: Project | null = existing ?? null;
      if (!target) {
        const added = (await api.projectCreate(pathBasename(path), path)) as Project;
        const list = (await api.projectsList()) as Project[];
        setProjects(list);
        target = list.find((p) => p.id === added.id) ?? added;
      }

      if (startChat) {
        await newChat(target);
        showToast(
          tr("composer.worktreeCreatedChat", {
            name: created.name,
            branch,
          }),
          2800,
        );
      } else {
        await bindSessionProject(target, { silent: true });
        showToast(
          tr("composer.worktreeCreated", {
            name: created.name,
            branch,
          }),
          2800,
        );
      }
    } catch (e) {
      setWorktreeCreateError(localizeUiError(e, locale));
    } finally {
      setWorktreeCreateBusy(false);
    }
  }, [
    activeProject?.path,
    bindSessionProject,
    newChat,
    projects,
    refreshGitWorktrees,
    showToast,
    tr,
    worktreeCreateName,
    worktreeCreateRef,
    worktreeCreateStartChat,
  ]);

  const resetAddProject = useCallback(() => {
    addProjectSourceRequestRef.current += 1;
    addProjectNameEditedRef.current = false;
    setAddProjectIntent(null);
    setAddProjectName("");
    setAddProjectPath("");
    setAddProjectError(null);
    setAddProjectBusy(false);
    setDragZone(null);
  }, []);

  const openAddProject = useCallback(
    (opts: { bindSession: boolean }, returnFocus?: HTMLElement | null) => {
      resetAddProject();
      setLocalError(null);
      addProjectReturnFocusRef.current =
        returnFocus ??
        (document.activeElement instanceof HTMLElement
          ? document.activeElement
          : null);
      setAddProjectIntent(opts);
    },
    [resetAddProject],
  );

  const closeAddProject = useCallback(() => {
    if (!addProjectBusy) resetAddProject();
  }, [addProjectBusy, resetAddProject]);

  const pickAddProjectDirectory = useCallback(async () => {
    setAddProjectError(null);
    if (!api.isTauri()) {
      setAddProjectError(tr("error.needTauri"));
      return;
    }
    const request = ++addProjectSourceRequestRef.current;
    try {
      const path = await api.pickDirectory();
      if (request === addProjectSourceRequestRef.current && path) {
        applyAddProjectSource(path);
      }
    } catch (error) {
      if (request === addProjectSourceRequestRef.current) {
        setAddProjectError(localizeUiError(error, locale));
      }
    }
  }, [applyAddProjectSource, locale, tr]);

  const submitAddProject = useCallback(async () => {
    const intent = addProjectIntent;
    const name = addProjectName.trim();
    if (!intent || addProjectBusy) return;
    if (!name) {
      setAddProjectError(tr("addProject.nameRequired"));
      addProjectNameRef.current?.focus();
      return;
    }
    const existing = addProjectPath
      ? projects.find((project) => pathsEqual(project.path, addProjectPath))
      : null;
    if (existing) {
      await finalizeAddedProject(existing, intent);
      resetAddProject();
      showToast(tr("addProject.existingSelected", { name: existing.name }));
      return;
    }
    setAddProjectBusy(true);
    setAddProjectError(null);
    try {
      const project = (await api.projectCreate(
        name,
        addProjectPath || null,
      )) as Project;
      await finalizeAddedProject(project, intent);
      resetAddProject();
    } catch (error) {
      setAddProjectError(localizeUiError(error, locale));
    } finally {
      setAddProjectBusy(false);
    }
  }, [
    addProjectBusy,
    addProjectIntent,
    addProjectName,
    addProjectPath,
    finalizeAddedProject,
    locale,
    projects,
    resetAddProject,
    showToast,
    tr,
  ]);

  const addProject = (returnFocus?: HTMLElement | null) =>
    openAddProject({ bindSession: false }, returnFocus);

  shortcutHandlersRef.current = {
    newChat: () => {
      void newChat();
    },
    openSettings: (section: SettingsSectionId = "general") => {
      navigateSettings(section);
    },
    openChatFind: () => {
      openChatFind();
    },
  };

  const availableUpdateVersion =
    appUpdateStatus?.latestRelease ?? appUpdateStatus?.latestVersion ?? "";
  const appUpdateAction = appUpdateActionFor(appUpdateStatus);
  const requestAppUpdateInstall = useCallback(() => {
    if (appUpdateAction !== "install") {
      setAppUpdateProgressOpen(true);
      return;
    }
    setAppUpdateProgressOpen(false);
    setAppDialog({
      kind: "confirm",
      title: tr("settings.updateConfirmTitle"),
      message: tr("settings.updateConfirm", {
        version: availableUpdateVersion,
      }),
      confirmLabel: tr("settings.updateInstall"),
      onConfirm: installAppUpdate,
    });
  }, [appUpdateAction, availableUpdateVersion, installAppUpdate, tr]);
  const sidebarUpdateLabel =
    appUpdateAction === "install"
      ? tr("sidebar.installUpdate", { version: availableUpdateVersion })
      : appUpdateAction === "retry"
        ? tr("settings.updateRetry")
        : tr("sidebar.updatePreparing", { version: availableUpdateVersion });
  // Agent 回合错误只进入对话气泡；顶部错误卡仅承载无法归属到回合的本地错误。
  const errorBanner = useMemo(
    () => presentErrorBanner(null, localError, locale),
    [localError, locale],
  );
  /** Prefer in-thread turn error; avoid stacking with the top error banner. */
  const hasChatTurnError = useMemo(
    () => messages.some((m) => m.isError),
    [messages],
  );
  // Collapse technical dump whenever the visible error changes.
  useEffect(() => {
    setErrorDetailOpen(false);
  }, [errorBanner?.code, errorBanner?.summary, errorBanner?.detail]);

  // T15: announce stream start/end once (avoid token-level noise).
  useEffect(() => {
    const streaming =
      session.state === "streaming" ||
      messages.some((m) => m.role === "assistant" && m.streaming);
    if (streaming && !wasStreamingRef.current) {
      setStreamA11yNote(tr("a11y.assistantStreaming"));
    } else if (!streaming && wasStreamingRef.current) {
      setStreamA11yNote(tr("a11y.assistantDone"));
      const t = window.setTimeout(() => setStreamA11yNote(""), 2500);
      wasStreamingRef.current = streaming;
      return () => window.clearTimeout(t);
    }
    wasStreamingRef.current = streaming;
  }, [session.state, messages, tr]);

  /** T04 错误卡片操作：重连、打开设置或关闭。 */
  const runErrorBannerAction = useCallback(
    (action: NonNullable<ErrorBannerView["primary"]>) => {
      setErrorDetailOpen(false);
      switch (action.id) {
        case "reconnect":
          setLocalError(null);
          void ensureConnected(true).then((sid) => {
            if (sid) setLocalError(null);
          });
          break;
        case "open_account":
          setLocalError(null);
          navigateSettings("account");
          break;
        case "open_providers":
          setLocalError(null);
          navigateSettings("account");
          break;
        case "dismiss":
        case "keep_waiting":
          // keep_waiting is for the stream-stall banner (clears prompt only).
          setLocalError(null);
          break;
        case "cancel_turn":
          setLocalError(null);
          void stop();
          break;
        default:
          break;
      }
    },
    [ensureConnected, navigateSettings, stop],
  );

  /** Export active (or given) session as Markdown (from PR #24). */
  const exportActiveSessionMd = useCallback(
    async (sessionMeta?: {
      id: string;
      title: string;
      projectId?: string | null;
    }) => {
      try {
        const id = sessionMeta?.id ?? session.sessionId;
        if (!id) {
          showToast(tr("session.exportFail"));
          return;
        }
        const title =
          sessionMeta?.title ||
          sessions.find((s) => s.id === id)?.title ||
          session.title ||
          tr("session.untitled");
        const projectId =
          sessionMeta?.projectId ??
          sessions.find((s) => s.id === id)?.projectId ??
          null;
        const proj =
          projects.find((p) => p.id === projectId) || activeProject || null;
        let msgs = messages;
        if (id !== session.sessionId) {
          msgs = projectPeriStoredMessages(await sessionMessages(id));
        }
        const md = sessionToMarkdown({
          title,
          projectName: proj?.name,
          projectPath: proj?.path,
          sessionId: id,
          messages: msgs.map((m) => ({
            role: m.role,
            content: m.content,
            thought: m.thought,
            createdAt: m.createdAt,
          })),
        });
        const blob = new Blob([md], { type: "text/markdown;charset=utf-8" });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = sessionExportFilename(title, id);
        a.click();
        URL.revokeObjectURL(url);
        showToast(tr("session.exportDone"));
      } catch (e) {
        showToast(`${tr("session.exportFail")}: ${String(e)}`);
      }
    },
    [
      session.sessionId,
      session.title,
      sessions,
      messages,
      projects,
      activeProject,
      showToast,
      tr,
    ],
  );

  return (
    <ImageViewerProvider locale={locale}>
    <div
      className={
        `app-shell platform-${platform}` +
        (windowMaximized ? " is-maximized" : "") +
        (windowFullscreen ? " is-fullscreen" : "") +
        (useCustomWindowChrome ? " has-custom-chrome" : "")
      }
      data-testid="app-shell"
    >
      <WindowControls
        visible={useCustomWindowChrome}
        labels={{
          minimize: tr("window.minimize"),
          maximize: tr("window.maximize"),
          restore: tr("window.restore"),
          close: tr("window.close"),
        }}
      />

      {wallpaperUrl && wallpaperRecord ? (
        <WallpaperMediaLayer
          url={wallpaperUrl}
          kind={wallpaperRecord.kind}
          focus={wallpaperRecord.focus ?? DEFAULT_WALLPAPER_FOCUS}
          clip={wallpaperRecord.clip ?? null}
          intrinsicSize={
            wallpaperRecord.width && wallpaperRecord.height
              ? { w: wallpaperRecord.width, h: wallpaperRecord.height }
              : null
          }
          onIntrinsicSize={applyWallpaperMediaSize}
        />
      ) : null}

      {appBooting ? (
        <StartupScreen useCustomWindowChrome={useCustomWindowChrome} />
      ) : appView === "settings" ? (
        <Suspense fallback={settingsPageFallback}>
        <SettingsPage
          section={settingsSection}
          onSection={navigateSettings}
          onBack={navigateWorkbench}
          locale={locale}
          onLocaleChange={(value) => {
            const previous = locale;
            setLocale(value);
            void api.settingsSet({ interfaceLanguage: value }).catch(() => {
              setLocale(previous);
              setToast(tr("settings.saveFailed"));
            });
          }}
          themePreference={themePreference}
          onTheme={applyThemeChoice}
          customInstructions={customInstructions}
          onCustomInstructionsSave={async (value) => {
            const saved = await api.customInstructionsSet(value);
            setCustomInstructions(saved);
          }}
          localMemories={localMemories}
          onLocalMemoriesChange={async (value) => {
            const saved = await api.settingsSet({ localMemories: value });
            setLocalMemories(saved.localMemories);
          }}
          memoryFile={memoryFile}
          onMemoryFileSave={async (value) => {
            const saved = await api.memoriesSet(value);
            setMemoryFile(saved);
          }}
          onMemoriesReset={async () => {
            await api.memoriesReset();
            setMemoryFile("");
            setToast(tr("settings.personalization.deleteMemoriesDone"));
          }}
          chromeHardwareAcceleration={chromeHardwareAcceleration}
          onChromeHardwareAcceleration={
            platform === "win"
              ? (value) => {
                  setChromeHardwareAcceleration(value);
                  void api.settingsSet({ chromeHardwareAcceleration: value });
                }
              : undefined
          }
          taskNotifications={taskNotifications}
          onTaskNotifications={(value) => {
            const previous = taskNotifications;
            setTaskNotifications(value);
            void api.settingsSet({ taskNotifications: value }).catch(() => {
              setTaskNotifications(previous);
              setToast(tr("settings.saveFailed"));
            });
          }}
          notificationSound={notificationSound}
          onNotificationSound={(value) => {
            const previous = notificationSound;
            setNotificationSound(value);
            void api.settingsSet({ notificationSound: value }).catch(() => {
              setNotificationSound(previous);
              setToast(tr("settings.saveFailed"));
            });
          }}
          appUpdateDownloadSource={appUpdateDownloadSource}
          onAppUpdateDownloadSource={(value) => {
            const previous = appUpdateDownloadSource;
            setAppUpdateDownloadSource(value);
            void api
              .settingsSet({ appUpdateDownloadSource: value })
              .catch(() => {
                setAppUpdateDownloadSource(previous);
                setToast(tr("settings.saveFailed"));
              });
          }}
          keepComputerAwake={keepComputerAwake}
          onKeepComputerAwake={(value) => {
            const previous = keepComputerAwake;
            setKeepComputerAwake(value);
            void api.settingsSet({ keepComputerAwake: value }).catch(() => {
              setKeepComputerAwake(previous);
              setToast(tr("settings.saveFailed"));
            });
          }}
          backgroundAgentLimit={backgroundAgentLimit}
          onBackgroundAgentLimit={(value) => {
            const previous = backgroundAgentLimit;
            setBackgroundAgentLimit(value);
            void api
              .settingsSet({ backgroundAgentLimit: value })
              .then((saved) => {
                setBackgroundAgentLimit(saved.backgroundAgentLimit);
              })
              .catch(() => {
                setBackgroundAgentLimit(previous);
                setToast(tr("settings.saveFailed"));
              });
          }}
          projectDirectory={projectDirectory}
          onProjectDirectoryChoose={async () => {
            const path = await api.pickDirectory();
            if (!path) return;
            const previous = projectDirectory;
            setProjectDirectory(path);
            try {
              const saved = await api.settingsSet({ projectDirectory: path });
              setProjectDirectory(saved.projectDirectory);
            } catch {
              setProjectDirectory(previous);
              setToast(tr("settings.saveFailed"));
            }
          }}
          onProjectDirectoryReset={async () => {
            const previous = projectDirectory;
            try {
              const path = await api.projectDefaultDirectory();
              setProjectDirectory(path);
              const saved = await api.settingsSet({ projectDirectory: path });
              setProjectDirectory(saved.projectDirectory);
            } catch {
              setProjectDirectory(previous);
              setToast(tr("settings.saveFailed"));
            }
          }}
          autoArchiveConversations={autoArchiveConversations ?? true}
          onAutoArchiveConversations={(value) => {
            const previous = autoArchiveConversations;
            setAutoArchiveConversations(value);
            void api.settingsSet({ autoArchiveConversations: value }).catch(() => {
              setAutoArchiveConversations(previous);
              setToast(tr("settings.saveFailed"));
            });
          }}
          archiveRetentionDays={archiveRetentionDays}
          onArchiveRetentionDays={(value) => {
            const previous = archiveRetentionDays;
            setArchiveRetentionDays(value);
            void api.settingsSet({ archiveRetentionDays: value }).catch(() => {
              setArchiveRetentionDays(previous);
              setToast(tr("settings.saveFailed"));
            });
          }}
          archivedSessions={sessions
            .filter((item) => item.archived)
            .map((item) => ({
              id: item.id,
              title: item.title,
              projectName: projects.find((project) => project.id === item.projectId)?.name ?? null,
              updatedAt: item.updatedAt,
            }))}
          onRestoreArchivedSession={async (sessionId) => {
            const archivedSession = sessions.find((item) => item.id === sessionId && item.archived);
            if (!archivedSession) return;
            await archiveSession(archivedSession, false);
          }}
          onDeleteArchivedSession={(sessionId) => {
            const archivedSession = sessions.find(
              (item) => item.id === sessionId && item.archived,
            );
            if (!archivedSession) return;
            setAppDialog({
              kind: "confirm",
              title: tr("settings.archived.deleteTitle"),
              message: tr("settings.archived.deleteConfirm", {
                title: archivedSession.title,
              }),
              confirmLabel: tr("settings.archived.delete"),
              danger: true,
              onConfirm: async () => {
                try {
                  await sessionDelete(archivedSession.id);
                  removeSessionPreference(archivedSession.id);
                  sendQueue.dropSessions([archivedSession.id]);
                  messagesBySessionRef.current.delete(archivedSession.id);
                  activeTurnIdBySessionRef.current.delete(archivedSession.id);
                  recoverableCompletedTurnIdBySessionRef.current.delete(
                    archivedSession.id,
                  );
                  completedTurnIdBySessionRef.current.delete(archivedSession.id);
                  turnLatencyBySessionRef.current.delete(archivedSession.id);
                  pendingVisibleTurnBySessionRef.current.delete(
                    archivedSession.id,
                  );
                  await refreshSessions();
                  showToast(tr("settings.archived.deleteDone"), 2800);
                } catch (error) {
                  setLocalError(localizeUiError(error, locale));
                }
              },
            });
          }}
          skin={skin}
          onSkin={applySkinChoice}
          wallpaperUrl={wallpaperUrl}
          wallpaperKind={wallpaperRecord?.kind ?? null}
          wallpaperFocus={wallpaperRecord?.focus ?? null}
          wallpaperClip={wallpaperRecord?.clip ?? null}
          wallpaperMediaSize={
            wallpaperRecord?.width && wallpaperRecord?.height
              ? { w: wallpaperRecord.width, h: wallpaperRecord.height }
              : null
          }
          onWallpaper={applyWallpaperChoice}
          onWallpaperAdjust={applyWallpaperAdjustChoice}
          onWallpaperMediaSize={applyWallpaperMediaSize}
          wallpaperScrim={wallpaperScrim}
          onWallpaperScrim={applyWallpaperScrimChoice}
          versionFooter={
            appUpdateStatus
              ? `KeenCode ${appUpdateStatus.currentRelease} · MIT`
              : tr("app.versionFooter")
          }
          appUpdateStatus={appUpdateStatus}
          appUpdateBusy={appUpdateBusy}
          appUpdateError={appUpdateError}
          onAppUpdateCheck={() => checkAppUpdate()}
          onAppUpdateInstall={requestAppUpdateInstall}
          projectPath={activeProject?.path ?? null}
          onProviderActivated={() => {
            void refreshProviderRoute()
              .then(() => {
                setProviderRouteRevision((revision) => revision + 1);
                showToast(tr("prov.switchedHotReload"), 3200);
              })
              .catch((error) => showToast(localizeUiError(error, locale), 4500));
          }}
        />
        </Suspense>
      ) : (
      <div className="workbench">
        {/* LEFT — fully hideable (not icon-rail); open via top-bar icon when closed */}
        <aside
          className={
            "sidebar" +
            (layout.sidebarCollapsed ? " sidebar--hidden" : "") +
            (resizingSidebar ? " is-resizing" : "")
          }
          aria-hidden={layout.sidebarCollapsed}
          style={
            !layout.sidebarCollapsed
              ? {
                  width: layout.sidebarWidth,
                  minWidth: layout.sidebarWidth,
                  maxWidth: layout.sidebarWidth,
                }
              : undefined
          }
        >
          {!layout.sidebarCollapsed && (
            <div
              className="sidebar-resizer"
              role="separator"
              aria-orientation="vertical"
              aria-label={tr("main.resizeLeftPane")}
              onPointerDown={(e) => {
                e.preventDefault();
                setResizingSidebar(true);
              }}
            />
          )}
          {/* Row 1: traffic-light height — panel toggle sits just right of traffic lights */}
          <div
            className="sidebar-chrome"
            data-tauri-drag-region
            onDoubleClick={() => {
              if (useCustomWindowChrome) void toggleMaximizeFromTitlebar();
            }}
          >
            <Tip label={tr("main.leftPaneHide")}>
              <Button
                type="button"
                className="chrome-btn chrome-btn--traffic main__pane-toggle is-on"
                onClick={() =>
                  setLayout((l) => {
                    const n = { ...l, sidebarCollapsed: true };
                    saveLayout(localStorage, n);
                    return n;
                  })
                }
              >
                <IconPanel size={16} />
              </Button>
            </Tip>
            <div className="sidebar-chrome__drag" data-tauri-drag-region />
          </div>

          {/* 主导航：新建任务、搜索，以及设置页技能/插件快捷入口。 */}
          <div className="sidebar-nav">
            <Button
              type="button"
              className="nav-new"
              onClick={() => void newChat(null)}
            >
              <span className="nav-item__icon">
                <IconNewChat size={18} />
              </span>
              {tr("sidebar.newSession")}
            </Button>
            <Button
              ref={searchTriggerRef}
              type="button"
              className="nav-new"
              onClick={openSearch}
            >
              <span className="nav-item__icon">
                <IconSearch size={18} />
              </span>
              {tr("sidebar.search")}
            </Button>
            <Button
              type="button"
              className="nav-new"
              onClick={() => navigateSettings("skills")}
            >
              <span className="nav-item__icon">
                <IconSkills size={18} />
              </span>
              {tr("sidebar.skills")}
            </Button>
            <Button
              type="button"
              className="nav-new"
              onClick={() => navigateSettings("market")}
            >
              <span className="nav-item__icon">
                <IconPuzzle size={18} />
              </span>
              {tr("sidebar.plugins")}
            </Button>
          </div>

          <OverlayScroll className="sidebar__scroll" viewportClassName="sidebar__scroll-inner">
            {/* 置顶任务独立展示，不在项目或普通任务栏目重复出现。无子项时隐藏栏目。 */}
            {pinnedSessions.length > 0 ? (
              <>
            <div className="tree-l1">
              <Button
                type="button"
                className="tree-l1__head"
                onClick={() => setPinnedOpen((value) => !value)}
                aria-expanded={pinnedOpen}
              >
                <span className="tree-l1__label">
                  {tr("sidebar.pinned")}
                </span>
                <IconChevronDown size={14} className="chevron--disclose" />
              </Button>
            </div>
            {pinnedOpen ? (
              <VirtualList
                className="tree-orphan-list"
                items={pinnedSessions}
                getKey={(item) => item.id}
                rowHeight={SIDEBAR_SESSION_ROW_HEIGHT}
                gap={SIDEBAR_SESSION_ROW_GAP}
                scrollToKey={
                  session.sessionId &&
                  pinnedSessions.some((item) => item.id === session.sessionId)
                    ? session.sessionId
                    : null
                }
                renderItem={(item) => {
                  const working = busyIds.has(item.id);
                  const completedUnread = completedUnreadIds.has(item.id);
                  const project = item.projectId
                    ? projects.find(
                        (candidate) => candidate.id === item.projectId,
                      ) ?? null
                    : null;
                  return (
                    <div
                      draggable
                      onDragStart={(event) => startSidebarDrag(event, "session", item.id)}
                      onDragEnd={endSidebarDrag}
                      onDragOver={(event) => event.preventDefault()}
                      onDrop={(event) => dropSession(event, item.id)}
                      className={
                        "tree-l3 tree-l3--orphan" +
                        (session.sessionId === item.id
                          ? " tree-l3--active"
                          : "") +
                        (working ? " tree-l3--working" : "") +
                        (pendingAskUserSessionIds.has(item.id)
                          ? " tree-l3--needs-input"
                          : "") +
                        (completedUnread
                          ? " tree-l3--completed-unread"
                          : "")
                      }
                      role="button"
                      tabIndex={0}
                      onClick={() => void openSession(item, project)}
                      onContextMenu={(event) =>
                        openSessionMenu(event, item)
                      }
                      onKeyDown={(event) => {
                        if (event.key === "Enter") {
                          void openSession(item, project);
                        }
                      }}
                    >
                      <span className="tree-l3__title">
                        <span
                          className="tree-l3__kind"
                          title={tr("session.pinned")}
                          aria-label={tr("session.pinned")}
                        >
                          <IconPin size={12} className="tree-l3__pin" />
                        </span>
                        <span className="tree-l3__name">
                          {item.title || "Untitled"}
                        </span>
                        {pendingAskUserSessionIds.has(item.id) ? (
                          <span className="tree-l3__input-badge">{tr("sidebar.needsUserInput")}</span>
                        ) : null}
                      </span>
                      {working ? (
                        <Spinner
                          size={14}
                          className="tree-l3__spinner"
                        />
                      ) : (
                        <>
                        {completedUnread ? (
                          <Tip label={tr("sidebar.sessionCompletedUnread")}>
                            <span
                              className="tree-l3__status tree-l3__status--completed"
                              aria-label={tr("sidebar.sessionCompletedUnread")}
                            >
                              <span className="tree-l3__completion-dot" />
                            </span>
                          </Tip>
                        ) : null}
                        <span className="tree-l3__actions tree-l3__actions--triple">
                          <Tip label={tr("session.unpin")}>
                            <Button
                              type="button"
                              className="tree-icon-btn"
                              onClick={(event) => {
                                event.stopPropagation();
                                void pinSession(item, false);
                              }}
                            >
                              <IconPinOff size={13} />
                            </Button>
                          </Tip>
                          <Tip label={tr("sidebar.archive")}>
                            <Button
                              type="button"
                              className="tree-icon-btn"
                              onClick={(event) => {
                                event.stopPropagation();
                                void archiveSession(item, true);
                              }}
                            >
                              <IconArchive size={13} />
                            </Button>
                          </Tip>
                          <Tip label={tr("sidebar.menu")}>
                            <Button
                              type="button"
                              className="tree-icon-btn"
                              onClick={(event) =>
                                openSessionMenu(event, item)
                              }
                            >
                              <IconMore size={13} />
                            </Button>
                          </Tip>
                        </span>
                        </>
                      )}
                    </div>
                  );
                }}
              />
            ) : null}
              </>
            ) : null}

            {/* L1 — Projects section */}
            <div className="tree-l1" style={{ marginTop: 8 }}>
              <Button
                type="button"
                className="tree-l1__head"
                onClick={() => setProjectsOpen((v) => !v)}
                aria-expanded={projectsOpen}
              >
                <span className="tree-l1__label">
                  {tr("sidebar.projects")}
                </span>
                <IconChevronDown size={14} className="chevron--disclose" />
              </Button>
              <div className="tree-l1__actions">
                {projects.length > 0 ? (
                  <Tip label={tr("sidebar.collapseAllProjects")}>
                    <Button
                      type="button"
                      className="tree-l1__action"
                      aria-label={tr("sidebar.collapseAllProjects")}
                      onClick={(e) => {
                        // Collapse each project folder only — not the L1 section.
                        e.stopPropagation();
                        setExpandedProjects((prev) => {
                          const next = { ...prev };
                          for (const p of projects) {
                            next[p.id] = false;
                          }
                          return next;
                        });
                      }}
                    >
                      <IconArrowsVerticalCollapse size={15} />
                    </Button>
                  </Tip>
                ) : null}
                <Tip label={tr("sidebar.addProject")}>
                  <Button
                    type="button"
                    className="tree-l1__action"
                    aria-label={tr("sidebar.addProject")}
                    onClick={() => void addProject()}
                  >
                    <IconPlus size={15} />
                  </Button>
                </Tip>
              </div>
            </div>

            {projectsOpen && projects.length === 0 && (
              <div className="sidebar-empty">
                {tr("sidebar.noProjects")}
              </div>
            )}

            {projectsOpen &&
              projects.map((proj) => {
                const open = expandedProjects[proj.id] !== false;
                const projSessions = sessionsForProject(proj.id);
                const visibleSessionCount =
                  visibleSessionsByProject[proj.id] ?? 5;
                const visibleSessions = projSessions.slice(
                  0,
                  visibleSessionCount,
                );
                return (
                  <div key={proj.id} className="tree-project">
                    {/* L2 — project folder: expand/collapse only (not selectable) */}
                    <div
                      draggable
                      onDragStart={(event) => startSidebarDrag(event, "project", proj.id)}
                      onDragEnd={endSidebarDrag}
                      onDragOver={(event) => dragOverProject(event, proj.id)}
                      onDragLeave={(event) => {
                        if (
                          !event.currentTarget.contains(
                            event.relatedTarget as Node | null,
                          )
                        ) {
                          setProjectDropHint(null);
                        }
                      }}
                      onDrop={(event) => dropProject(event, proj.id)}
                      className={
                        "tree-l2" +
                        (projectDropHint?.id === proj.id
                          ? projectDropHint.after
                            ? " tree-l2--drop-after"
                            : " tree-l2--drop-before"
                          : "") +
                        (isProjectPathMissing(proj.pathOk)
                          ? " tree-l2--path-missing"
                          : "")
                      }
                      role="button"
                      tabIndex={0}
                      aria-expanded={open}
                      aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown"
                      onClick={() => {
                        setExpandedProjects((e) => ({
                          ...e,
                          [proj.id]: !open,
                        }));
                      }}
                      onContextMenu={(e) => openProjectMenu(e, proj)}
                      onKeyDown={(e) => {
                        if (
                          e.altKey &&
                          (e.key === "ArrowUp" || e.key === "ArrowDown")
                        ) {
                          e.preventDefault();
                          const index = projects.findIndex(
                            (project) => project.id === proj.id,
                          );
                          const moveDown = e.key === "ArrowDown";
                          const target = projects[index + (moveDown ? 1 : -1)];
                          if (target) {
                            const ids = moveId(
                              projects.map(({ id }) => id),
                              proj.id,
                              target.id,
                              moveDown,
                            );
                            applyProjectOrder(ids);
                            showToast(
                              tr("sidebar.projectMoved", {
                                name: proj.name,
                                position: ids.indexOf(proj.id) + 1,
                                total: ids.length,
                              }),
                            );
                          }
                          return;
                        }
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          setExpandedProjects((ex) => ({
                            ...ex,
                            [proj.id]: !open,
                          }));
                        }
                      }}
                    >
                      <span className="tree-l2__icon">
                        {open ? (
                          <IconFolderOpen size={17} />
                        ) : (
                          <IconFolder size={17} />
                        )}
                      </span>
                      <Tip
                        label={
                          isProjectPathMissing(proj.pathOk)
                            ? tr("project.pathMissing", { name: proj.name })
                            : proj.path
                        }
                      >
                        <span className="tree-l2__name">
                          {proj.name}
                        </span>
                      </Tip>
                      {isProjectPathMissing(proj.pathOk) ? (
                        <span className="project-row__badge project-row__badge--path-missing">
                          {tr("sidebar.pathMissing")}
                        </span>
                      ) : null}
                      <span className="tree-l2__actions">
                        <Tip label={tr("sidebar.newConversation")}>
                          <Button
                            type="button"
                            className="tree-icon-btn"
                            disabled={isProjectPathMissing(proj.pathOk)}
                            onClick={(e) => {
                              e.stopPropagation();
                              void newChat(proj);
                            }}
                          >
                            <IconSquarePen size={14} />
                          </Button>
                        </Tip>
                        <Tip label={tr("sidebar.menu")}>
                          <Button
                            type="button"
                            className="tree-icon-btn"
                            onClick={(e) => openProjectMenu(e, proj)}
                          >
                            <IconMore size={14} />
                          </Button>
                        </Tip>
                      </span>
                    </div>

                    {open && (
                      <div className="tree-l3-list-wrap">
                        {isProjectPathMissing(proj.pathOk) && (
                          <Button
                            type="button"
                            className="tree-l3 tree-l3--hint"
                            onClick={(e) => {
                              e.stopPropagation();
                              void relocateProject(proj);
                            }}
                          >
                            {tr("sidebar.relocateProject")}
                          </Button>
                        )}
                        {projSessions.length > 0 ? (
                          <VirtualList
                            className="tree-l3-list"
                            items={visibleSessions}
                            getKey={(s) => s.id}
                            rowHeight={SIDEBAR_SESSION_ROW_HEIGHT}
                            gap={SIDEBAR_SESSION_ROW_GAP}
                            scrollToKey={
                              session.sessionId &&
                              visibleSessions.some((x) => x.id === session.sessionId)
                                ? session.sessionId
                                : null
                            }
                            renderItem={(s) => {
                              const working = busyIds.has(s.id);
                              const completedUnread = completedUnreadIds.has(s.id);
                              return (
                                <div
                                  draggable
                                  onDragStart={(event) => startSidebarDrag(event, "session", s.id)}
                                  onDragEnd={endSidebarDrag}
                                  onDragOver={(event) => event.preventDefault()}
                                  onDrop={(event) => dropSession(event, s.id)}
                                  className={
                                    "tree-l3" +
                                    (session.sessionId === s.id
                                      ? " tree-l3--active"
                                      : "") +
                                    (s.archived ? " tree-l3--archived" : "") +
                                    (working ? " tree-l3--working" : "") +
                                    (pendingAskUserSessionIds.has(s.id)
                                      ? " tree-l3--needs-input"
                                      : "") +
                                    (completedUnread
                                      ? " tree-l3--completed-unread"
                                      : "")
                                  }
                                  role="button"
                                  tabIndex={0}
                                  onClick={() => void openSession(s, proj)}
                                  onContextMenu={(e) => openSessionMenu(e, s)}
                                  onKeyDown={(e) => {
                                    if (e.key === "Enter")
                                      void openSession(s, proj);
                                  }}
                                >
                                  <span className="tree-l3__title">
                                    {s.pinned ? (
                                      <span
                                        className="tree-l3__kind"
                                        title={tr("session.pinned")}
                                        aria-label={tr("session.pinned")}
                                      >
                                        <IconPin
                                          size={12}
                                          className="tree-l3__pin"
                                        />
                                      </span>
                                    ) : null}
                                    <span className="tree-l3__name">
                                      {s.title || "Untitled"}
                                    </span>
                                    {pendingAskUserSessionIds.has(s.id) ? (
                                      <span className="tree-l3__input-badge">{tr("sidebar.needsUserInput")}</span>
                                    ) : null}
                                  </span>
                                  {working ? (
                                    <Tip label={tr("sidebar.sessionWorking")}>
                                      <span
                                        className="tree-l3__status"
                                        aria-label={tr(
                                          "sidebar.sessionWorking",
                                        )}
                                      >
                                        <Spinner
                                          size={14}
                                          className="tree-l3__spinner"
                                        />
                                      </span>
                                    </Tip>
                                  ) : (
                                    <>
                                    {completedUnread ? (
                                      <Tip label={tr("sidebar.sessionCompletedUnread")}>
                                        <span
                                          className="tree-l3__status tree-l3__status--completed"
                                          aria-label={tr("sidebar.sessionCompletedUnread")}
                                        >
                                          <span className="tree-l3__completion-dot" />
                                        </span>
                                      </Tip>
                                    ) : null}
                                    <span className="tree-l3__actions tree-l3__actions--triple">
                                      <Tip
                                        label={
                                          s.pinned
                                            ? tr("session.unpin")
                                            : tr("session.pin")
                                        }
                                      >
                                        <Button
                                          type="button"
                                          className="tree-icon-btn"
                                          onClick={(e) => {
                                            e.stopPropagation();
                                            void pinSession(s, !s.pinned);
                                          }}
                                        >
                                          {s.pinned ? (
                                            <IconPinOff size={13} />
                                          ) : (
                                            <IconPin size={13} />
                                          )}
                                        </Button>
                                      </Tip>
                                      <Tip
                                        label={
                                          s.archived
                                            ? tr("sidebar.unarchive")
                                            : tr("sidebar.archive")
                                        }
                                      >
                                        <Button
                                          type="button"
                                          className="tree-icon-btn"
                                          onClick={(e) => {
                                            e.stopPropagation();
                                            void archiveSession(
                                              s,
                                              !s.archived,
                                            );
                                          }}
                                        >
                                          <IconArchive size={13} />
                                        </Button>
                                      </Tip>
                                      <Tip label={tr("sidebar.menu")}>
                                        <Button
                                          type="button"
                                          className="tree-icon-btn"
                                          onClick={(e) =>
                                            openSessionMenu(e, s)
                                          }
                                        >
                                          <IconMore size={13} />
                                        </Button>
                                      </Tip>
                                    </span>
                                    </>
                                  )}
                                </div>
                              );
                            }}
                          />
                        ) : null}
                        {projSessions.length > visibleSessionCount ? (
                          <Button
                            type="button"
                            className="tree-l3-more"
                            onClick={() =>
                              setVisibleSessionsByProject((counts) => ({
                                ...counts,
                                [proj.id]: visibleSessionCount + 5,
                              }))
                            }
                          >
                            {tr("sidebar.showMore")}
                          </Button>
                        ) : null}
                        {projSessions.length === 0 && (
                          <div className="sidebar-empty" style={{ padding: "4px 10px" }}>
                            {tr("sidebar.noChats")}
                          </div>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}

            {/* 无项目任务栏目；无子项时隐藏。 */}
            {orphanSessions.length > 0 ? (
              <>
            <div className="tree-l1" style={{ marginTop: 8 }}>
              <Button
                type="button"
                className="tree-l1__head"
                onClick={() => setHistoryOpen((v) => !v)}
                aria-expanded={historyOpen}
              >
                <span className="tree-l1__label">
                  {tr("sidebar.otherSessions")}
                </span>
                <IconChevronDown size={14} className="chevron--disclose" />
              </Button>
            </div>
            {historyOpen ? (
              <VirtualList
                className="tree-orphan-list"
                items={orphanSessions}
                getKey={(s) => s.id}
                rowHeight={SIDEBAR_SESSION_ROW_HEIGHT}
                gap={SIDEBAR_SESSION_ROW_GAP}
                scrollToKey={
                  session.sessionId &&
                  orphanSessions.some((x) => x.id === session.sessionId)
                    ? session.sessionId
                    : null
                }
                renderItem={(s) => {
                  const working = busyIds.has(s.id);
                  const completedUnread = completedUnreadIds.has(s.id);
                  return (
                    <div
                      draggable
                      onDragStart={(event) => startSidebarDrag(event, "session", s.id)}
                      onDragEnd={endSidebarDrag}
                      onDragOver={(event) => event.preventDefault()}
                      onDrop={(event) => dropSession(event, s.id)}
                      className={
                        "tree-l3 tree-l3--orphan" +
                        (session.sessionId === s.id
                          ? " tree-l3--active"
                          : "") +
                        (working ? " tree-l3--working" : "") +
                        (pendingAskUserSessionIds.has(s.id)
                          ? " tree-l3--needs-input"
                          : "") +
                        (completedUnread
                          ? " tree-l3--completed-unread"
                          : "")
                      }
                      role="button"
                      tabIndex={0}
                      onClick={() => void openSession(s)}
                      onContextMenu={(e) => openSessionMenu(e, s)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") void openSession(s);
                      }}
                    >
                      <span className="tree-l3__title">
                        {s.pinned ? (
                          <span
                            className="tree-l3__kind"
                            title={tr("session.pinned")}
                            aria-label={tr("session.pinned")}
                          >
                            <IconPin
                              size={12}
                              className="tree-l3__pin"
                            />
                          </span>
                        ) : null}
                        <span className="tree-l3__name">
                          {s.title || "Untitled"}
                        </span>
                        {pendingAskUserSessionIds.has(s.id) ? (
                          <span className="tree-l3__input-badge">{tr("sidebar.needsUserInput")}</span>
                        ) : null}
                      </span>
                      {working ? (
                        <Tip label={tr("sidebar.sessionWorking")}>
                          <span
                            className="tree-l3__status"
                            aria-label={tr("sidebar.sessionWorking")}
                          >
                            <Spinner
                              size={14}
                              className="tree-l3__spinner"
                            />
                          </span>
                        </Tip>
                      ) : (
                        <>
                        {completedUnread ? (
                          <Tip label={tr("sidebar.sessionCompletedUnread")}>
                            <span
                              className="tree-l3__status tree-l3__status--completed"
                              aria-label={tr("sidebar.sessionCompletedUnread")}
                            >
                              <span className="tree-l3__completion-dot" />
                            </span>
                          </Tip>
                        ) : null}
                        <span className="tree-l3__actions tree-l3__actions--triple">
                          <Tip
                            label={
                              s.pinned
                                ? tr("session.unpin")
                                : tr("session.pin")
                            }
                          >
                            <Button
                              type="button"
                              className="tree-icon-btn"
                              onClick={(e) => {
                                e.stopPropagation();
                                void pinSession(s, !s.pinned);
                              }}
                            >
                              {s.pinned ? (
                                <IconPinOff size={13} />
                              ) : (
                                <IconPin size={13} />
                              )}
                            </Button>
                          </Tip>
                          <Tip label={tr("sidebar.archive")}>
                            <Button
                              type="button"
                              className="tree-icon-btn"
                              onClick={(e) => {
                                e.stopPropagation();
                                void archiveSession(s, !s.archived);
                              }}
                            >
                              <IconArchive size={13} />
                            </Button>
                          </Tip>
                          <Button
                            type="button"
                            className="tree-icon-btn"
                            onClick={(e) => openSessionMenu(e, s)}
                          >
                            <IconMore size={13} />
                          </Button>
                        </span>
                        </>
                      )}
                    </div>
                  );
                }}
              />
            ) : null}
              </>
            ) : null}
          </OverlayScroll>

          <UserMenu
            labels={{
              settings: tr("sidebar.settings"),
              update: sidebarUpdateLabel,
            }}
            updateAvailable={appUpdateStatus?.available === true}
            updateBusy={appUpdateBusy !== null}
            onSettings={() => navigateSettings("general")}
            onUpdate={requestAppUpdateInstall}
          />
        </aside>

        {/* CENTER — solid pane; top icons fully toggle L/R columns */}
        <main
          className={
            "main" +
            (layout.sidebarCollapsed ? " main--sidebar-hidden" : "") +
            (layout.asideCollapsed ? " main--aside-hidden" : "") +
            (dragZone === "main" ? " is-drop-target" : "")
          }
        >
          {dragZone === "main" && (
            <div className="drop-overlay drop-overlay--attach" aria-hidden>
              <div className="drop-overlay__card">
                <span className="drop-overlay__icon">
                  <IconAttach size={22} />
                </span>
                <strong>{tr("composer.dropAttachTitle")}</strong>
                <span>{tr("composer.dropAttachHint")}</span>
              </div>
            </div>
          )}
          {toast && (
            <div className="app-toast" role="status">
              {toast}
            </div>
          )}
          <div
            className="main__top"
            data-tauri-drag-region
            onDoubleClick={() => {
              if (useCustomWindowChrome) void toggleMaximizeFromTitlebar();
            }}
          >
            <div className="main__title-row" data-tauri-drag-region>
              {layout.sidebarCollapsed && (
                <>
                  <Tip label={tr("main.leftPaneShow")}>
                    <Button
                      type="button"
                      className="chrome-btn chrome-btn--traffic main__pane-toggle"
                      onClick={() =>
                        setLayout((l) => {
                          const n = { ...l, sidebarCollapsed: false };
                          saveLayout(localStorage, n);
                          return n;
                        })
                      }
                    >
                      <IconPanel size={16} />
                    </Button>
                  </Tip>
                  <Tip label={tr("sidebar.newSession")}>
                    <Button
                      type="button"
                      className="chrome-btn chrome-btn--traffic"
                      onClick={() => void newChat(null)}
                    >
                      <IconNewChat size={16} />
                    </Button>
                  </Tip>
                </>
              )}
              {(() => {
                const cur = sessions.find((s) => s.id === session.sessionId);
                const title = cur?.title || session.title || "";
                if (
                  isPlaceholderSessionTitle(title, [
                    tr("session.new"),
                    tr("session.placeholderTitle"),
                  ])
                ) {
                  return null;
                }
                return (
                  <>
                    <Tip label={title}>
                      <h1 className="main__title" data-tauri-drag-region>
                        {title}
                      </h1>
                    </Tip>
                    {cur && (
                      <Tip label={tr("session.menu")}>
                        <Button
                          type="button"
                          className="chrome-btn main__title-menu"
                          onClick={(e) => openSessionMenu(e, cur)}
                        >
                          <IconMore size={16} />
                        </Button>
                      </Tip>
                    )}
                  </>
                );
              })()}
            </div>
            {session.sessionId ? (
              <div className="main__top-actions">
              <Tip
                label={
                  summaryOpen
                    ? tr("main.summaryHide")
                    : tr("main.summaryShow")
                }
              >
                <Button
                  ref={summaryTriggerRef}
                  type="button"
                  className={
                    "chrome-btn main__pane-toggle" +
                    (summaryOpen ? " is-on" : "")
                  }
                  aria-pressed={summaryOpen}
                  onClick={() => setSummaryOpen((value) => !value)}
                >
                  <IconSummary size={16} />
                </Button>
              </Tip>
              <Tip
                    label={
                      layout.asideCollapsed
                        ? tr("main.rightPaneShow")
                        : tr("main.rightPaneHide")
                    }
                  >
                    <Button
                      type="button"
                      className={
                        "chrome-btn main__pane-toggle" +
                        (!layout.asideCollapsed ? " is-on" : "")
                      }
                      onClick={() =>
                        setLayout((l) => {
                          const n = {
                            ...l,
                            asideCollapsed: !l.asideCollapsed,
                          };
                          saveLayout(localStorage, n);
                          return n;
                        })
                      }
                    >
                      <IconPanelRight size={16} />
                    </Button>
              </Tip>
              </div>
            ) : null}
          </div>

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

          {/* I06: soft stall — heal-first Host; soft banner is secondary. Primary = keep waiting. */}
          {streamStall && (
            <div
              className={`stall-banner error-banner${
                (() => {
                  const sid = streamStall.sessionId || session.sessionId || "";
                  const live = liveMap[sid];
                  const saw =
                    !!streamStall.sawModelOutput ||
                    !!live?.sawModelOutput ||
                    false;
                  const tools =
                    !!streamStall.sawToolActivity ||
                    !!live?.sawToolActivity ||
                    false;
                  const hostTier = normalizeStallTier(streamStall.tier);
                  const tier =
                    hostTier ??
                    stallTierFromProgress({
                      sawModelOutput: saw,
                      sawToolActivity: tools,
                      terminalCandidate: saw && !live?.liveToolId,
                    });
                  return tier === "maybe_done" || tier === "post_output"
                    ? " stall-banner--soft"
                    : "";
                })()
              }`}
              role="status"
            >
              <div className="error-banner__code">STREAM_STALL</div>
              <div className="error-banner__summary">
                {(() => {
                  const sid = streamStall.sessionId || session.sessionId || "";
                  const live = liveMap[sid];
                  const saw =
                    !!streamStall.sawModelOutput || !!live?.sawModelOutput;
                  const tools =
                    !!streamStall.sawToolActivity || !!live?.sawToolActivity;
                  const hostTier = normalizeStallTier(streamStall.tier);
                  const tier =
                    hostTier ??
                    stallTierFromProgress({
                      sawModelOutput: saw,
                      sawToolActivity: tools,
                      terminalCandidate: saw && !live?.liveToolId,
                    });
                  const key = stallMessageKey(tier);
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
                })()}
              </div>
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
          )}

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
              onQueryChange={(q) => {
                setChatFindQuery(q);
                setChatFindIndex(0);
              }}
              onPrev={chatFindPrev}
              onNext={chatFindNext}
              onClose={() => setShowChatFind(false)}
            />
          )}
          {/* Pre-turn / host errors: T04 deck (problem · cause · primary · secondary) */}
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
                    onClick={() => {
                      if (errorBanner.primary) {
                        runErrorBannerAction(errorBanner.primary);
                      }
                    }}
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
                    onClick={() => {
                      if (errorBanner.secondary) {
                        runErrorBannerAction(errorBanner.secondary);
                      }
                    }}
                  >
                    {errorBanner.secondary.label}
                  </Button>
                ) : null}
                {!errorBanner.primary &&
                  (errorBanner.reconnectHint ||
                    session.state === "disconnected") && (
                    <Button
                      type="button"
                      className="btn btn--ghost error-banner__reconnect"
                      disabled={connecting}
                      onClick={() => {
                        setLocalError(null);
                        setErrorDetailOpen(false);
                        void ensureConnected(true).then((sid) => {
                          if (sid) setLocalError(null);
                        });
                      }}
                    >
                      {tr("main.reconnect")}
                    </Button>
                  )}
                {errorBanner.detail ? (
                  <Button
                    type="button"
                    className="error-banner__details-btn"
                    aria-expanded={errorDetailOpen}
                    onClick={() => setErrorDetailOpen((v) => !v)}
                  >
                    {errorDetailOpen
                      ? tr("error.hideDetails")
                      : tr("error.details")}
                  </Button>
                ) : null}
              </div>
              {errorBanner.detail && errorDetailOpen && (
                <pre className="error-banner__detail">{errorBanner.detail}</pre>
              )}
            </div>
          )}

          <div
            className={
              "main__stage" +
              (summaryOpen ? " main__stage--summary-open" : "")
            }
            style={
              {
                ["--composer-float-pad"]: `${composerFloatPad}px`,
              } as CSSProperties
            }
          >
          <div className="sr-only" aria-live="polite" aria-atomic="true">
            {streamA11yNote}
          </div>
          {/* 新任务空态标题悬浮在居中输入区上方；草稿长高后会压到
              输入文字，所以一旦有内容就不再渲染该引导。 */}
          <ConversationThread
            locale={locale}
            messages={messages}
            sessionState={
              stopLatch.phase === "force_idle" ? "ready" : session.state
            }
            sessionKey={session.sessionId ?? `draft-${session.title ?? "new"}`}
            projectPath={activeProject?.path ?? null}
            turnStartedAt={turnStartedAt}
            retryStatus={retryStatus}
            suppressEmptyCopy={!showWelcomeCopy}
            onOpenSessionChanges={() => {
              setLayout((l) => {
                if (l.asideCollapsed) {
                  const n = { ...l, asideCollapsed: false };
                  saveLayout(localStorage, n);
                  return n;
                }
                return l;
              });
              setResourceOpenTarget({ type: "changes" });
            }}
            onOpenModifiedPath={(path) => {
              setLayout((l) => {
                if (l.asideCollapsed) {
                  const n = { ...l, asideCollapsed: false };
                  saveLayout(localStorage, n);
                  return n;
                }
                return l;
              });
              setResourceOpenTarget({ type: "changes", path });
            }}
            onOpenResource={(target) => {
              setLayout((l) => {
                if (l.asideCollapsed) {
                  const n = { ...l, asideCollapsed: false };
                  saveLayout(localStorage, n);
                  return n;
                }
                return l;
              });
              setResourceOpenTarget(target);
            }}
            onAddAttachmentToComposer={(att) =>
              setAttachments((prev) => mergeAttachments(prev, [att]))
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
              setLayout((current) => {
                if (!current.asideCollapsed) return current;
                const next = { ...current, asideCollapsed: false };
                saveLayout(localStorage, next);
                return next;
              });
              setResourceOpenTarget({ type: "subagent", agentId });
            }}
            onOpenSubagentList={() => {
              setLayout((current) => {
                if (!current.asideCollapsed) return current;
                const next = { ...current, asideCollapsed: false };
                saveLayout(localStorage, next);
                return next;
              });
              setResourceOpenTarget({ type: "subagents" });
            }}
            onOpenChanges={() => {
              setLayout((current) => {
                if (!current.asideCollapsed) return current;
                const next = { ...current, asideCollapsed: false };
                saveLayout(localStorage, next);
                return next;
              });
              setResourceOpenTarget({ type: "changes" });
            }}
          />

          {askUser ? (
            <div ref={askUserWrapRef} className="ask-user-wrap">
              <AskUserModal
                payload={askUser}
                labels={{
                  title: tr("askUser.title"), submit: tr("askUser.submit"),
                  next: tr("askUser.next"), cancel: tr("askUser.cancel"),
                  otherPlaceholder: tr("askUser.otherPlaceholder"),
                  freeTextHint: tr("askUser.freeTextHint"),
                  multiHint: tr("askUser.multiHint"), close: tr("common.close"),
                }}
                onSubmit={async (answers) => {
                  const payload = askUser;
                  if (!payload) return;
                  try {
                    await sessionResolveAskUser({
                      decision: "accepted",
                      answers: toElicitationAnswers(payload, answers),
                      rpcId: payload.rpcId,
                    });
                    clearPendingAskUser(payload.sessionId, payload.rpcId);
                    setAskUser((current) => current?.rpcId === payload.rpcId ? null : current);
                  } catch (error) {
                    showToast(localizeUiError(error, locale), 4500);
                  }
                }}
                onCancel={async () => {
                  const payload = askUser;
                  if (!payload) return;
                  try {
                    await sessionResolveAskUser({ decision: "cancelled", rpcId: payload.rpcId });
                  } catch { /* 取消后仍关闭当前卡片。 */ }
                  clearPendingAskUser(payload.sessionId, payload.rpcId);
                  setAskUser((current) => current?.rpcId === payload.rpcId ? null : current);
                }}
              />
            </div>
          ) : null}

          <div
            ref={composerWrapRef}
            className={
              "composer-wrap composer-wrap--float" +
              (welcomeSession ? " composer-wrap--welcome" : "")
            }
          >
            <div
              className={
                "composer-stack" +
                (welcomeSession
                  ? " composer-stack--with-context"
                  : "")
              }
            >
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
              {/* 新任务始终展示项目选择；选择项目后再展示对应 Worktree。 */}
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
                      worktreesUnavailable: tr(
                        "composer.worktreesUnavailable",
                      ),
                      worktreesLoading: tr("composer.worktreesLoading"),
                      worktreeCurrent: tr("composer.worktreeCurrent"),
                      worktreeMain: tr("composer.worktreeMain"),
                      worktreeDetached: tr("composer.worktreeDetached"),
                      worktreeTip: tr("composer.worktreeTip"),
                      worktreeNew: tr("composer.worktreeNew"),
                      worktreeNewChat: tr("composer.worktreeNewChat"),
                      worktreeGc: tr("composer.worktreeGc"),
                    }}
                    onSwitch={(wt) => {
                      void switchToWorktree(wt);
                    }}
                    onCreate={() => openWorktreeCreate()}
                    onCreateAndChat={() =>
                      openWorktreeCreate({ startNewChat: true })
                    }
                    onGc={openWorktreeGc}
                    onOpen={refreshGitWorktrees}
                  />
                ) : null}
              </div>
              ) : null}
            <div
              ref={composerShellRef}
              className={
                "composer" +
                (dragZone === "main" ? " composer--drop-ready" : "")
              }
            >
              {sendQueue.activeQueue.length > 0 && (
                <div
                  className="composer__queue"
                  aria-label={tr("composer.queueCount", {
                    n: String(sendQueue.activeQueue.length),
                  })}
                >
                  <div className="composer__queue-head">
                    <IconClock size={14} aria-hidden />
                    <span className="composer__queue-title">
                      {tr("composer.queueCount", {
                        n: String(sendQueue.activeQueue.length),
                      })}
                    </span>
                    <Button
                      type="button"
                      className="composer__queue-clear"
                      disabled={sendQueue.steeringIds.size > 0}
                      onClick={sendQueue.clearQueue}
                    >
                      {tr("composer.queueClear")}
                    </Button>
                  </div>
                  {sendQueue.flushHold ? (
                    <div className="composer__queue-hold" role="status">
                      <span className="composer__queue-hold-text">
                        {tr("composer.queueHold")}
                      </span>
                      <Button
                        type="button"
                        className="composer__queue-hold-retry"
                        onClick={() => sendQueue.resumeFlush()}
                      >
                        {tr("composer.queueHoldRetry")}
                      </Button>
                    </div>
                  ) : null}
                  <ul className="composer__queue-list">
                    {sendQueue.activeQueue.map((item, idx) => (
                      <li key={item.id} className="composer__queue-item">
                        <span className="composer__queue-idx" aria-hidden>
                          {idx + 1}
                        </span>
                        <span
                          className="composer__queue-text"
                          title={queuePreviewText(
                            item.storedDisplay,
                            item.attachments,
                            200,
                            queuePreviewLabels,
                          )}
                        >
                          {queuePreviewText(
                            item.storedDisplay,
                            item.attachments,
                            72,
                            queuePreviewLabels,
                          )}
                        </span>
                        <Button
                          type="button"
                          className="composer__queue-steer"
                          disabled={
                            session.state !== "streaming" ||
                            sendQueue.steeringIds.has(item.id)
                          }
                          onClick={() => {
                            void sendQueue
                              .steerItem(item.id, steerQueuedItem)
                              .catch((error: unknown) =>
                                showToast(localizeUiError(error, locale), 4000),
                              );
                          }}
                        >
                          {sendQueue.steeringIds.has(item.id)
                            ? tr("composer.queueSteering")
                            : tr("composer.queueSteer")}
                        </Button>
                        <Button
                          type="button"
                          className="composer__queue-remove"
                          aria-label={tr("composer.queueRemove")}
                          disabled={sendQueue.steeringIds.has(item.id)}
                          onClick={() => sendQueue.removeItem(item.id)}
                        >
                          <IconClose size={12} />
                        </Button>
                      </li>
                    ))}
                  </ul>
                </div>
              )}
              {attachments.length > 0 && (
                <div
                  className="composer__attachments"
                  aria-label={tr("composer.attachCount", {
                    n: String(attachments.length),
                  })}
                >
                  {attachments.map((a) => (
                    <AttachmentCard
                      key={a.path}
                      attachment={a}
                      variant="chip"
                      labels={attachLabels}
                      galleryPaths={attachments
                        .filter((x) => !x.isDir && isImagePath(x.path))
                        .map((x) => x.path)}
                      onRemove={(att) =>
                        setAttachments((prev) =>
                          prev.filter((x) => x.path !== att.path),
                        )
                      }
                      onAddToComposer={(att) =>
                        setAttachments((prev) => mergeAttachments(prev, [att]))
                      }
                    />
                  ))}
                </div>
              )}
              {composerMenuOpen &&
                composerPlusPos &&
                typeof document !== "undefined" &&
                createPortal(
                  <ComposerPlusPanel
                    open
                    panelRef={composerPlusPanelRef}
                    locale={locale}
                    entries={composerMenuEntries}
                    filterQuery={
                      liveSlash.present ? slashFilterQuery : undefined
                    }
                    skillsLoading={skillsLoading}
                    activeIndex={slashActiveIndex}
                    onActiveIndexChange={setSlashActiveIndex}
                    onSelectUpload={() => {
                      void pickComposerFiles();
                    }}
                    onSelectSlash={applySlashItem}
                    resolveTitle={resolveSlashTitle}
                    resolveDescription={resolveSlashDescription}
                    style={{
                      ...composerPlusStyle,
                      zIndex: 10050,
                    }}
                  />,
                  document.body,
                )}
              {promptHistoryOpen &&
                promptHistoryPos &&
                typeof document !== "undefined" &&
                createPortal(
                  <PromptHistoryPanel
                    open
                    panelRef={promptHistoryPanelRef}
                    entries={promptHistoryEntries}
                    query={promptHistoryFilter}
                    activeIndex={promptHistoryActive}
                    focusFilter={promptHistoryFocusFilter}
                    labels={{
                      title: tr("promptHistory.title"),
                      placeholder: tr("promptHistory.placeholder"),
                      empty: tr("promptHistory.empty"),
                      emptyFilter: tr("promptHistory.emptyFilter"),
                      aria: tr("promptHistory.aria"),
                    }}
                    onQueryChange={setPromptHistoryFilter}
                    onActiveIndexChange={(i) => {
                      setPromptHistoryActive(i);
                      const entry = promptHistoryEntries[i];
                      if (entry && !promptHistoryFocusFilter) {
                        // 空输入按上键时逐条浏览当前任务的历史提示。
                        applyPromptHistoryEntry(entry, {
                          close: false,
                          listIndex: i,
                        });
                      }
                    }}
                    onSelect={(entry) => applyPromptHistoryEntry(entry)}
                    onClose={closePromptHistory}
                    style={{
                      ...promptHistoryStyle,
                      zIndex: 10050,
                    }}
                  />,
                  document.body,
                )}
              <ComposerEditor
                editorRef={composerInputRef}
                className="composer__input"
                value={draft}
                disabled={!canType(session.state)}
                placeholder={tr("composer.placeholder")}
                onChange={(next) => {
                  setDraft(next);
                  // Manual edit exits history browse; same text (DOM re-sync) keeps it.
                  const idx = promptHistoryIndexRef.current;
                  if (idx !== null) {
                    const hist = collectUserPromptHistory(messages);
                    if (next !== hist[idx]) {
                      promptHistoryIndexRef.current = null;
                      setPromptHistoryIndex(null);
                      // Keep the picker open so the user can re-pick; only leave browse index.
                    }
                  }
                }}
                onSlashQueryChange={onSlashQueryChange}
                onPasteFiles={(files) => void addPastedFiles(files)}
                onPastePaths={(paths) => void addAttachmentsFromPaths(paths)}
                onKeyDown={(e) => {
                  if (
                    e.nativeEvent.isComposing ||
                    (e.nativeEvent as KeyboardEvent).keyCode === 229
                  ) {
                    return;
                  }
                  if (composerMenuOpen) {
                    // Ref = same array the panel renders (never desync).
                    const flat = composerMenuEntriesRef.current;
                    const n = flat.length;
                    if (e.key === "ArrowDown") {
                      e.preventDefault();
                      if (!n) return;
                      setSlashActiveIndex((i) => (i + 1) % n);
                      return;
                    }
                    if (e.key === "ArrowUp") {
                      e.preventDefault();
                      if (!n) return;
                      setSlashActiveIndex((i) => (i - 1 + n) % n);
                      return;
                    }
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      const entry =
                        flat[
                          Math.min(
                            Math.max(0, slashActiveIndex),
                            Math.max(0, n - 1),
                          )
                        ];
                      if (!entry) return;
                      if (entry.kind === "upload") void pickComposerFiles();
                      else applySlashItem(entry.item);
                      return;
                    }
                    if (e.key === "Escape") {
                      e.preventDefault();
                      closeComposerMenu();
                      return;
                    }
                    if (e.key === "Tab" && n > 0) {
                      e.preventDefault();
                      const entry =
                        flat[
                          Math.min(
                            Math.max(0, slashActiveIndex),
                            n - 1,
                          )
                        ]!;
                      if (entry.kind === "upload") void pickComposerFiles();
                      else applySlashItem(entry.item);
                      return;
                    }
                  }
                  // Prompt history picker open: ↑/↓ move selection; Enter/Tab apply;
                  // Esc closes (Build `/history` + empty-↑).
                  if (promptHistoryOpenRef.current && !composerMenuOpen) {
                    if (e.key === "Escape") {
                      e.preventDefault();
                      closePromptHistory();
                      return;
                    }
                    if (e.key === "Enter" || e.key === "Tab") {
                      const entry = promptHistoryEntries[promptHistoryActive];
                      if (entry) {
                        e.preventDefault();
                        applyPromptHistoryEntry(entry, {
                          listIndex: promptHistoryActive,
                        });
                        return;
                      }
                    }
                    if (e.key === "ArrowUp" || e.key === "ArrowDown") {
                      e.preventDefault();
                      if (promptHistoryEntries.length === 0) return;
                      if (e.key === "ArrowUp") {
                        const next = Math.min(
                          promptHistoryActive + 1,
                          promptHistoryEntries.length - 1,
                        );
                        setPromptHistoryActive(next);
                        const entry = promptHistoryEntries[next];
                        if (entry) {
                          applyPromptHistoryEntry(entry, {
                            close: false,
                            listIndex: next,
                          });
                        }
                        return;
                      }
                      // ArrowDown: newer; past newest closes like Build.
                      if (promptHistoryActive <= 0) {
                        promptHistoryIndexRef.current = null;
                        setPromptHistoryIndex(null);
                        setDraft("");
                        closePromptHistory();
                        return;
                      }
                      const next = promptHistoryActive - 1;
                      setPromptHistoryActive(next);
                      const entry = promptHistoryEntries[next];
                      if (entry) {
                        applyPromptHistoryEntry(entry, {
                          close: false,
                          listIndex: next,
                        });
                      }
                      return;
                    }
                  }
                  // 提示词历史：空草稿中按 ↑ 打开选择器并定位到最新一项。
                  // Only when slash palette is closed so palette ↑/↓ is untouched.
                  if (
                    (e.key === "ArrowUp" || e.key === "ArrowDown") &&
                    !composerMenuOpen &&
                    !promptHistoryOpenRef.current
                  ) {
                    const history = collectUserPromptHistory(messages);
                    const draftEmpty = isDraftEmpty(parseStoredContent(draft));
                    const browsing = promptHistoryIndexRef.current !== null;
                    if (
                      shouldHandlePromptHistoryKey({
                        key: e.key,
                        draftEmpty,
                        browsing,
                        historyLength: history.length,
                      })
                    ) {
                      e.preventDefault();
                      if (e.key === "ArrowUp" && !browsing) {
                        openPromptHistory({
                          focusFilter: false,
                          seedDraft: true,
                        });
                        return;
                      }
                      const step = stepPromptHistory(
                        history,
                        promptHistoryIndexRef.current,
                        e.key === "ArrowUp" ? "up" : "down",
                      );
                      promptHistoryIndexRef.current = step.index;
                      setPromptHistoryIndex(step.index);
                      setDraft(step.text);
                      if (step.index == null) {
                        closePromptHistory();
                      } else if (!promptHistoryOpenRef.current) {
                        openPromptHistory({
                          focusFilter: false,
                          seedDraft: false,
                        });
                        setPromptHistoryActive(step.index);
                      } else {
                        setPromptHistoryActive(step.index);
                      }
                      return;
                    }
                  }
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    const hasBody =
                      !isDraftEmpty(parseStoredContent(draft)) ||
                      attachments.length > 0;
                    if (hasBody && hasConfiguredModel) {
                      void send();
                    }
                  }
                  if (e.key === "Escape") {
                    if (promptHistoryOpenRef.current) {
                      closePromptHistory();
                      return;
                    }
                    closeComposerMenu();
                  }
                }}
              />
              <div className="composer__row">
                <Tip label={tr("composer.add")}>
                  <Button
                    ref={composerPlusTriggerRef}
                    type="button"
                    className={
                      "icon-btn icon-btn--plus" +
                      (composerMenuOpen ? " is-open" : "")
                    }
                    aria-label={tr("composer.add")}
                    onClick={() => {
                      if (composerMenuOpen) {
                        closeComposerMenu();
                      } else {
                        setShowComposerPlus(true);
                      }
                    }}
                  >
                    <IconPlus size={18} />
                  </Button>
                </Tip>
                {(acpSessionView?.session_id === session.sessionId &&
                  acpSessionView.goal.goal) ||
                goalModeSessionKey === (session.sessionId ?? "__draft__") ? (
                  <ComposerGoalChip
                    locale={locale}
                    onClear={
                      acpSessionView?.session_id === session.sessionId &&
                      acpSessionView.goal.goal
                        ? () => {
                            setGoalModeSessionKey(null);
                            confirmClearCurrentGoal();
                          }
                        : () => setGoalModeSessionKey(null)
                    }
                  />
                ) : null}
                {/* 计划 chip 与目标 chip 同逻辑：仅在模式激活时出现，
                    入口是 /plan 命令；点击 chip 关闭模式。 */}
                {planModeSessionKey === (session.sessionId ?? "__draft__") ? (
                  <ComposerPlanModeChip
                    locale={locale}
                    active={true}
                    onToggle={() => setPlanModeSessionKey(null)}
                  />
                ) : null}
                <ComposerModelMenu
                  open={composerPanel === "model"}
                  onOpenChange={(open) =>
                    setComposerPanel((current) =>
                      open ? "model" : current === "model" ? null : current,
                    )
                  }
                  providerId={activeCustomProvider?.id}
                  modelId={modelId}
                  models={availableModels}
                  labels={{
                    model: tr("composer.model"),
                    addModel: tr("composer.addModel"),
                  }}
                  onModel={(v, providerId) => {
                    if (!isValidModelId(v, availableModels)) return;
                    setModelId(v);
                    if (api.isTauri() && providerId) {
                      const activeSessionId = viewingSessionIdRef.current;
                      if (activeSessionId) {
                        // 会话级切换（Q1）：只改当前会话的 provider，
                        // 不动新会话默认值，也不重置会话视图。
                        modelBySessionRef.current.set(activeSessionId, v);
                        void sessionSetModel({
                          sessionId: activeSessionId,
                          providerId,
                          modelId: v,
                        }).catch((e: unknown) => {
                          modelBySessionRef.current.delete(activeSessionId);
                          showToast(localizeUiError(e, locale), 4000);
                        });
                      } else {
                        void api
                          .providersSelectModel(providerId, v)
                          .then(() => refreshProviderRoute())
                          .catch((e) =>
                            showToast(localizeUiError(e, locale), 4000),
                          );
                      }
                    }
                  }}
                  onAddModel={() => navigateSettings("account")}
                />
                <ComposerReasoningMenu
                  open={composerPanel === "reasoning"}
                  onOpenChange={(open) =>
                    setComposerPanel((current) =>
                      open
                        ? "reasoning"
                        : current === "reasoning"
                          ? null
                          : current,
                    )
                  }
                  model={availableModels.find(
                    (model) =>
                      model.id === modelId &&
                      (!activeCustomProvider?.id ||
                        model.providerId === activeCustomProvider.id),
                  )}
                  effort={effort}
                  ultra={
                    ultraModeSessionKey ===
                    (session.sessionId ?? "__draft__")
                  }
                  labels={{
                    reasoning: tr("composer.effort"),
                    reasoningUnsupported: tr("composer.reasoningUnsupported"),
                    ultra: tr("composer.ultra"),
                    ultraDescription: tr("composer.ultraDescription"),
                    effortNone: tr("effort.none"),
                    effortMinimal: tr("effort.minimal"),
                    effortHigh: tr("effort.high"),
                    effortMedium: tr("effort.medium"),
                    effortLow: tr("effort.low"),
                    effortXHigh: tr("effort.xhigh"),
                    effortMax: tr("effort.max"),
                  }}
                  onEffort={(v) => {
                    const activeModel = availableModels.find(
                      (model) =>
                        model.id === modelId &&
                        (!activeCustomProvider?.id ||
                          model.providerId === activeCustomProvider.id),
                    );
                    if (!isValidEffort(v, activeModel)) return;
                    setEffort(v);
                    const activeSessionId = viewingSessionIdRef.current;
                    if (api.isTauri() && activeSessionId) {
                      void sessionSetEffort({
                        sessionId: activeSessionId,
                        effort: v,
                      }).catch((e: unknown) =>
                        showToast(localizeUiError(e, locale), 4000),
                      );
                    }
                  }}
                  onUltra={(enabled) => {
                    const key = session.sessionId ?? "__draft__";
                    setUltraModeSessionKey(enabled ? key : null);
                  }}
                />
                {hasStartedConversation ? (
                  <ContextUsageChip
                    display={contextUsageDisplay}
                    taskCacheUsage={
                      taskCacheUsage?.sessionId === session.sessionId
                        ? taskCacheUsage
                        : null
                    }
                    labels={{
                      aria: tr("context.chipAria"),
                      contextUsageRate: tr("context.usageRate"),
                      taskCacheHitRate: tr("context.taskCacheHitRate"),
                    }}
                  />
                ) : null}
                <span className="composer__spacer" />
                {effectiveCanStop ? (
                  <>
                    {hasConfiguredModel &&
                      (!isDraftEmpty(parseStoredContent(draft)) ||
                        attachments.length > 0) &&
                      shouldEnqueueSend(session.state, connecting) && (
                      <Tip label={tr("composer.send")}>
                        <Button
                          type="button"
                          className="icon-btn icon-btn--primary"
                          onClick={() => void send()}
                          aria-label={tr("composer.send")}
                        >
                          <IconSend size={16} />
                        </Button>
                      </Tip>
                    )}
                    <Tip label={tr("composer.stop")}>
                      <Button
                        type="button"
                        className="icon-btn icon-btn--danger"
                        onClick={() => void stop()}
                        aria-label={tr("composer.stop")}
                      >
                        <IconStop size={14} />
                      </Button>
                    </Tip>
                  </>
                ) : (
                  <Tip label={tr("composer.send")}>
                    <Button
                      type="button"
                      className="icon-btn icon-btn--primary"
                      disabled={
                        !hasConfiguredModel ||
                        (!effectiveCanSend &&
                          !shouldEnqueueSend(session.state, connecting)) ||
                        (isDraftEmpty(parseStoredContent(draft)) &&
                          attachments.length === 0)
                      }
                      onClick={() => void send()}
                      aria-label={tr("composer.send")}
                    >
                      <IconSend size={16} />
                    </Button>
                  </Tip>
                )}
              </div>
            </div>
            </div>
          </div>
          </div>
        </main>

        {/* RIGHT — session-linked project resource viewer (fully hideable + resizable) */}
        <aside
          className={
            (layout.asideCollapsed ? "aside aside--hidden" : "aside") +
            (resizingAside ? " is-resizing" : "")
          }
          aria-hidden={layout.asideCollapsed}
          style={
            !layout.asideCollapsed
              ? {
                  width: layout.asideWidth,
                  minWidth: layout.asideWidth,
                  maxWidth: layout.asideWidth,
                }
              : undefined
          }
        >
          {!layout.asideCollapsed && (
            <div
              className="aside-resizer"
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize files pane"
              onPointerDown={(e) => {
                e.preventDefault();
                setResizingAside(true);
              }}
            />
          )}
          <div className="aside__inner">
            <ResourceViewer
              sessionKey={session.sessionId ?? "__draft__"}
              projectPath={activeProject?.path ?? null}
              projectName={activeProject?.name ?? null}
              locale={locale}
              paneActive={!layout.asideCollapsed}
              onTabsEmpty={() =>
                setLayout((current) => {
                  if (current.asideCollapsed) return current;
                  const next = { ...current, asideCollapsed: true };
                  saveLayout(localStorage, next);
                  return next;
                })
              }
              syncRevision={resourceSyncRevision}
              openRequest={resourceOpenTarget}
              onOpenRequestConsumed={() => setResourceOpenTarget(null)}
              trajectoryLive={{
                sessionId: session.sessionId ?? null,
                title: acpSessionView?.title ?? null,
                messages,
                subagents: displayedSubagents,
              }}
              subagents={displayedSubagents}
              modelLabel={modelLabel}
              subagentModelLabels={subagentModelLabels}
              onLoadTrajectoryMessages={loadTrajectoryMessages}
            />
          </div>
        </aside>
      </div>
      )}

      <GlassModal
        open={addProjectIntent !== null}
        onClose={closeAddProject}
        title={tr("addProject.title")}
        titleId="add-project-title"
        size="lg"
        className="add-project-modal"
        overlayClassName="add-project-overlay"
        closeLabel={tr("common.close")}
        closeOnOverlay={!addProjectBusy}
        showClose={!addProjectBusy}
        wrapBody
        returnFocusRef={addProjectReturnFocusRef}
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              disabled={addProjectBusy}
              onClick={closeAddProject}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              type="submit"
              form="add-project-form"
              className="btn btn--solid"
              disabled={
                addProjectBusy
              }
            >
              {addProjectBusy ? <Spinner size={14} /> : null}
              {addProjectBusy
                ? tr("addProject.adding")
                : tr("addProject.submit")}
            </Button>
          </>
        }
      >
        <form
          id="add-project-form"
          className="add-project-form"
          onSubmit={(event) => {
            event.preventDefault();
            void submitAddProject();
          }}
        >
          <div className="add-project-field">
            <Label htmlFor="add-project-name" className="prov-field__label">
              {tr("addProject.name")}
            </Label>
            <div className="add-project-name-control">
              <IconFolder size={17} />
              <Input
                ref={addProjectNameRef}
                id="add-project-name"
                className="settings-input"
                value={addProjectName}
                placeholder={tr("addProject.namePlaceholder")}
                maxLength={120}
                autoComplete="off"
                data-modal-autofocus
                readOnly={addProjectBusy}
                aria-invalid={
                  addProjectError === tr("addProject.nameRequired") || undefined
                }
                aria-describedby={addProjectError ? "add-project-error" : undefined}
                onChange={(event) => {
                  addProjectNameEditedRef.current = true;
                  setAddProjectName(event.target.value);
                  setAddProjectError(null);
                }}
              />
            </div>
          </div>

          <div className="add-project-field">
            <Label htmlFor="add-project-source" className="prov-field__label">
              {tr("addProject.sourceFolder")}
            </Label>
            <Button
              ref={addProjectDropRef}
              id="add-project-source"
              type="button"
              className={
                "cpm__action add-project-drop" +
                (dragZone === "project" ? " is-active" : "")
              }
              disabled={addProjectBusy}
              onClick={() => void pickAddProjectDirectory()}
              aria-label={
                addProjectPath
                  ? pathBasename(addProjectPath)
                  : tr("addProject.chooseFolder")
              }
            >
              <IconFolderPlus size={24} />
              <strong className="add-project-drop__title">
                {addProjectPath
                  ? pathBasename(addProjectPath)
                  : tr("addProject.chooseFolder")}
              </strong>
              {addProjectPath ? (
                <span
                  className="add-project-drop__path"
                  title={addProjectPath}
                >
                  {addProjectPath}
                </span>
              ) : null}
            </Button>
            {!addProjectPath && projectDirectory ? (
              <div className="add-project-default-path settings-row__desc">
                <span>
                  {tr("addProject.defaultLocation", {
                    path: projectPathPreview(
                      projectDirectory,
                      addProjectName.trim() || tr("addProject.namePlaceholder"),
                    ),
                  })}
                </span>
                <Button
                  type="button"
                  className="add-project-default-path__settings btn btn--ghost btn--sm"
                  onClick={() => {
                    resetAddProject();
                    navigateSettings("general");
                  }}
                >
                  {tr("addProject.changeDefaultLocation")}
                </Button>
              </div>
            ) : null}
          </div>

          {addProjectError ? (
            <p
              id="add-project-error"
              className="ext-alert ext-alert--error"
              role="alert"
            >
              {addProjectError}
            </p>
          ) : null}
        </form>
      </GlassModal>

      <GlassModal
        open={appUpdateProgressOpen}
        onClose={() => setAppUpdateProgressOpen(false)}
        title={tr("settings.updateTitle")}
        size="sm"
        closeLabel={tr("common.close")}
        closeOnOverlay={false}
        wrapBody
      >
        <AppUpdateProgress
          locale={locale}
          status={appUpdateStatus}
          installing={appUpdateBusy === "installing"}
          error={appUpdateError}
          onRetry={checkAppUpdate}
          onInstall={requestAppUpdateInstall}
        />
      </GlassModal>

      <GlassModal
        open={worktreeCreateOpen}
        onClose={() => {
          if (worktreeCreateBusy) return;
          setWorktreeCreateOpen(false);
        }}
        title={
          worktreeCreateStartChat
            ? tr("composer.worktreeNewChatTitle")
            : tr("composer.worktreeNewTitle")
        }
        size="sm"
        closeLabel={tr("common.close")}
        closeOnOverlay={!worktreeCreateBusy}
        showClose={!worktreeCreateBusy}
        wrapBody
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              disabled={worktreeCreateBusy}
              onClick={() => setWorktreeCreateOpen(false)}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              type="button"
              className="btn btn--solid"
              disabled={worktreeCreateBusy || !worktreeCreateName.trim()}
              onClick={() => {
                void submitWorktreeCreate();
              }}
            >
              {worktreeCreateBusy
                ? tr("composer.worktreeCreating")
                : worktreeCreateStartChat
                  ? tr("composer.worktreeCreateChat")
                  : tr("composer.worktreeCreate")}
            </Button>
          </>
        }
      >
        <form
          className="wt-create"
          onSubmit={(e) => {
            e.preventDefault();
            if (worktreeCreateBusy) return;
            void submitWorktreeCreate();
          }}
        >
          <p className="wt-create__hint">
            {worktreeCreateStartChat
              ? tr("composer.worktreeNewChatHint")
              : tr("composer.worktreeNewHint")}
          </p>
          <Label className="wt-create__field">
            <span className="wt-create__label">
              {tr("composer.worktreeName")}
            </span>
            <Input
              className="settings-input"
              value={worktreeCreateName}
              onChange={(e) => {
                setWorktreeCreateName(e.target.value);
                setWorktreeCreateError(null);
              }}
              placeholder={tr("composer.worktreeNamePlaceholder")}
              autoComplete="off"
              autoFocus
              disabled={worktreeCreateBusy}
              spellCheck={false}
            />
          </Label>
          <Label className="wt-create__field">
            <span className="wt-create__label">
              {tr("composer.worktreeRef")}
            </span>
            <Input
              className="settings-input"
              value={worktreeCreateRef}
              onChange={(e) => {
                setWorktreeCreateRef(e.target.value);
                setWorktreeCreateError(null);
              }}
              placeholder={tr("composer.worktreeRefPlaceholder")}
              autoComplete="off"
              disabled={worktreeCreateBusy}
              spellCheck={false}
            />
          </Label>
          {worktreeCreatePreviewPath ? (
            <p className="wt-create__preview">
              {tr("composer.worktreePathPreview", {
                path: worktreeCreatePreviewPath,
              })}
            </p>
          ) : null}
          {worktreeCreateError ? (
            <p className="wt-create__error" role="alert">
              {worktreeCreateError}
            </p>
          ) : null}
        </form>
      </GlassModal>
      <GlassModal
        open={worktreeGcOpen}
        onClose={() => {
          if (worktreeGcBusy) return;
          setWorktreeGcOpen(false);
          setWorktreeGcError(null);
          setWorktreeGcPreview(null);
          setWorktreeGcForce(false);
        }}
        title={tr("composer.worktreeGcTitle")}
        size="sm"
        closeLabel={tr("common.close")}
        closeOnOverlay={!worktreeGcBusy}
        showClose={!worktreeGcBusy}
        wrapBody
        footer={
          <>
            <Button
              type="button"
              className="btn btn--ghost"
              disabled={worktreeGcBusy}
              onClick={() => {
                setWorktreeGcOpen(false);
                setWorktreeGcError(null);
                setWorktreeGcPreview(null);
                setWorktreeGcForce(false);
              }}
            >
              {tr("common.cancel")}
            </Button>
            <Button
              type="button"
              className="btn btn--solid"
              disabled={worktreeGcBusy || worktreeGcPreviewBusy}
              onClick={() => {
                void submitWorktreeGc();
              }}
            >
              {worktreeGcBusy
                ? tr("composer.worktreeGcRunning")
                : tr("composer.worktreeGcConfirm")}
            </Button>
          </>
        }
      >
        <div className="wt-gc">
          <p className="wt-gc__hint">{tr("composer.worktreeGcHint")}</p>
          <div className="wt-gc__force">
            <Checkbox
              id="worktree-gc-force"
              checked={worktreeGcForce}
              disabled={worktreeGcBusy || worktreeGcPreviewBusy}
              onCheckedChange={(checked) =>
                setWorktreeGcForce(checked === true)
              }
              aria-labelledby="worktree-gc-force-label"
            />
            <Label htmlFor="worktree-gc-force" id="worktree-gc-force-label">
              {tr("composer.worktreeGcForce")}
            </Label>
          </div>
          <div className="wt-gc__preview-head">{tr("composer.worktreeGcPreview")}</div>
          {worktreeGcPreviewBusy ? (
            <p className="wt-gc__preview-status">
              {tr("composer.worktreeGcPreviewLoading")}
            </p>
          ) : worktreeGcPreview ? (
            <>
              {(worktreeGcPreview.prunable?.length ?? 0) > 0 ? (
                <p className="wt-gc__prunable">
                  {tr("composer.worktreeGcPrunable", {
                    n: String(worktreeGcPreview.prunable?.length ?? 0),
                  })}
                </p>
              ) : null}
              {(worktreeGcPreview.output ?? "").trim() ||
              (worktreeGcPreview.prunable?.length ?? 0) > 0 ? (
                <pre className="wt-gc__output" tabIndex={0}>
                  {(worktreeGcPreview.output ?? "").trim() ||
                    (Array.isArray(worktreeGcPreview.prunable)
                      ? worktreeGcPreview.prunable.join("\n")
                      : "")}
                </pre>
              ) : (
                <p className="wt-gc__preview-status">
                  {tr("composer.worktreeGcPreviewEmpty")}
                </p>
              )}
            </>
          ) : worktreeGcError ? null : (
            <p className="wt-gc__preview-status">
              {tr("composer.worktreeGcPreviewEmpty")}
            </p>
          )}
          {worktreeGcError ? (
            <p className="wt-gc__error" role="alert">
              {worktreeGcError}
            </p>
          ) : null}
        </div>
      </GlassModal>
      <GlassModal
        open={showShortcuts}
        onClose={() => setShowShortcuts(false)}
        title={tr("shortcuts.title")}
        size="md"
        closeLabel={tr("shortcuts.close")}
        footer={
          <Button
            type="button"
            className="btn btn--ghost"
            onClick={() => setShowShortcuts(false)}
          >
            {tr("shortcuts.close")}
          </Button>
        }
      >
        <ul className="shortcuts-list">
          {shortcutsForPlatform(
            platform === "mac" ? "mac" : platform === "win" ? "win" : "other",
          ).map((row) => (
              <li key={row.id} className="shortcuts-list__row">
                <span className="shortcuts-list__label">
                  {tr(row.labelKey as MessageKey)}
                </span>
                <kbd className="shortcuts-list__keys">{row.keys}</kbd>
              </li>
            ))}
        </ul>
      </GlassModal>
      <StatusModal
        open={showStatusModal}
        locale={locale}
        sessionId={session.sessionId}
        modelId={modelId}
        effort={
          availableModels
            .find((model) => model.id === modelId)
            ?.reasoningEfforts?.some((entry) => entry.id === effort)
            ? effort
            : null
        }
        projectPath={activeProject?.path}
        messageCount={messages.length}
        onClose={() => setShowStatusModal(false)}
      />
      {/* 搜索面板挂载到 body，避免 WebView2 将其参与工作台弹性布局。 */}
      {showSearch &&
        typeof document !== "undefined" &&
        createPortal(
        <div
          className="overlay search-overlay"
          onClick={() => setShowSearch(false)}
        >
          <div
            className="search-panel"
            onClick={(e) => e.stopPropagation()}
            role="dialog"
            aria-label={tr("sidebar.search")}
          >
            <div className="search-panel__head">
              <IconSearch size={16} />
              <Input
                autoFocus
                className="search-panel__input"
                placeholder={
                  tr("search.placeholder")
                }
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
              />
              <Button
                type="button"
                className="icon-btn modal-close"
                onClick={() => setShowSearch(false)}
                aria-label={tr("common.close")}
              >
                <IconClose size={16} />
              </Button>
            </div>
            {searchHits.matchedProjects.length > 0 && (
              <>
                <div className="search-panel__section">
                  {tr("sidebar.projects")}
                </div>
                {searchHits.matchedProjects.map((p) => (
                  <Button
                    key={p.id}
                    type="button"
                    className="search-panel__row"
                    onClick={() => {
                      setShowSearch(false);
                      // Project is a folder: expand only; selection is for sessions.
                      setProjectsOpen(true);
                      setExpandedProjects((e) => ({ ...e, [p.id]: true }));
                    }}
                  >
                    <IconFolder size={15} />
                    <span className="search-panel__title">{p.name}</span>
                    <span className="search-panel__meta">{p.path}</span>
                  </Button>
                ))}
              </>
            )}
            <div className="search-panel__section">
              {tr("search.chats")}
            </div>
            {searchHits.matchedSessions.length === 0 && (
              <div className="sidebar-empty" style={{ padding: 12 }}>
                {tr("search.noMatches")}
              </div>
            )}
            {searchHits.matchedSessions.map((hit, i) => {
              const row = sessions.find((sessionRow) => sessionRow.id === hit.id);
              if (!row) return null;
              const proj = projects.find(
                (project) => project.id === row.projectId,
              );
              const metaParts: string[] = [];
              if (proj?.name) metaParts.push(proj.name);
              if (i < 9) metaParts.push(`⌘${i + 1}`);
              return (
                <Button
                  key={hit.id}
                  type="button"
                  className="search-panel__row"
                  onClick={() => {
                    setShowSearch(false);
                    void openSession(row, proj ?? null);
                  }}
                >
                  <IconSquarePen size={15} />
                  <span className="search-panel__body">
                    <span className="search-panel__title">
                      {row.title}
                    </span>
                  </span>
                  <span className="search-panel__meta">
                    {metaParts.join(" · ") || "—"}
                  </span>
                </Button>
              );
            })}
            <div className="search-panel__foot">
              <Button
                type="button"
                className="search-panel__row"
                onClick={() => {
                  setShowSearch(false);
                  void newChat(activeProject);
                }}
              >
                <IconSquarePen size={15} />
                <span className="search-panel__title">
                  {tr("search.newChat")}
                </span>
              </Button>
              <Button
                type="button"
                className="search-panel__row"
                onClick={() => {
                  setShowSearch(false);
                  void addProject(searchReturnFocusRef.current);
                }}
              >
                <IconFolder size={15} />
                <span className="search-panel__title">
                  {tr("sidebar.addProject")}
                </span>
              </Button>
            </div>
          </div>
        </div>,
        document.body,
      )}

      {/* 应用内确认与输入框；Tauri WebView 不可靠支持 window.prompt/confirm。 */}
      {appDialog &&
        typeof document !== "undefined" &&
        createPortal(
          <div
            className="overlay app-dialog-overlay"
            role="presentation"
            onMouseDown={(e) => {
              if (e.target === e.currentTarget) setAppDialog(null);
            }}
          >
            <div
              className="modal app-dialog"
              role="dialog"
              aria-modal="true"
              aria-labelledby="app-dialog-title"
              onMouseDown={(e) => e.stopPropagation()}
            >
              <header className="modal-head">
                <h2 id="app-dialog-title" className="modal-title">
                  {appDialog.title}
                </h2>
                <Button
                  type="button"
                  className="icon-btn modal-close"
                  onClick={() => setAppDialog(null)}
                  aria-label={tr("common.close")}
                >
                  <IconClose size={16} />
                </Button>
              </header>
              {appDialog.kind === "confirm" ? (
                <form
                  className="app-dialog__form"
                  onSubmit={(e) => {
                    e.preventDefault();
                    // Prefer the keyboard path's latest ref so chained
                    // chained dialogs stay consistent.
                    const dialog = appDialogRef.current;
                    if (!dialog || dialog.kind !== "confirm") return;
                    const run = dialog.onConfirm;
                    setAppDialog(null);
                    void run();
                  }}
                >
                  <p className="app-dialog__msg">{appDialog.message}</p>
                  <div className="app-dialog__actions modal-actions">
                    <Button
                      type="button"
                      className="btn btn--ghost"
                      onClick={() => setAppDialog(null)}
                    >
                      {tr("common.cancel")}
                    </Button>
                    <Button
                      ref={confirmBtnRef}
                      type="submit"
                      className={`btn ${appDialog.danger ? "btn--danger" : "btn--solid"}`}
                    >
                      {appDialog.confirmLabel || tr("common.confirm")}
                    </Button>
                  </div>
                </form>
              ) : (
                <form
                  className="app-dialog__form"
                  onSubmit={(e) => {
                    e.preventDefault();
                    const value = dialogInput;
                    const submit = appDialog.onSubmit;
                    setAppDialog(null);
                    void submit(value);
                  }}
                >
                  {appDialog.message ? (
                    <p className="app-dialog__msg">{appDialog.message}</p>
                  ) : null}
                  <Input
                    ref={dialogInputRef}
                    className="app-dialog__input"
                    value={dialogInput}
                    placeholder={appDialog.placeholder}
                    onChange={(e) => setDialogInput(e.target.value)}
                    autoComplete="off"
                  />
                  <div className="app-dialog__actions modal-actions">
                    <Button
                      type="button"
                      className="btn btn--ghost"
                      onClick={() => setAppDialog(null)}
                    >
                      {tr("common.cancel")}
                    </Button>
                    <Button type="submit" className="btn btn--solid">
                      {appDialog.submitLabel || tr("common.save")}
                    </Button>
                  </div>
                </form>
              )}
            </div>
          </div>,
          document.body,
        )}

      {/* Floating context menu (project / session) — unified ContextMenu */}
      {(() => {
        let items: ContextMenuItem[] = [];
        if (ctxMenu?.kind === "project") {
          const proj = projects.find((p) => p.id === ctxMenu.id);
          if (proj) {
            items = [
              {
                id: "reveal",
                label: tr("project.reveal"),
                icon: <IconExternalLink size={16} />,
                onClick: () => {
                  void api
                    .projectReveal(proj.id)
                    .catch((e) => setLocalError(localizeUiError(e, locale)));
                },
              },
              {
                id: "relocate",
                label: tr("project.relocate"),
                icon: <IconFolderPlus size={16} />,
                onClick: () => {
                  void relocateProject(proj);
                },
              },
              {
                id: "rename",
                label: tr("project.rename"),
                icon: <IconRename size={16} />,
                onClick: () => renameProject(proj),
              },
              {
                id: "remove",
                label: tr("project.remove"),
                icon: <IconTrash size={16} />,
                danger: true,
                onClick: () => removeProjectFromApp(proj),
              },
            ];
          }
        } else if (ctxMenu?.kind === "session") {
          const s = sessions.find((x) => x.id === ctxMenu.id);
          if (s) {
            items = [
              {
                id: "rename",
                label: tr("session.rename"),
                icon: <IconRename size={16} />,
                onClick: () => renameSession(s),
              },
              {
                id: "fork",
                label: tr("session.fork"),
                icon: <IconFork size={16} />,
                onClick: () => confirmForkSession(s),
              },
              {
                id: "trajectory",
                label: tr("session.viewTrajectory"),
                icon: <IconListTree size={16} />,
                onClick: () => viewTrajectory(s),
              },
              {
                id: "copy-id",
                label: tr("session.copyId"),
                icon: <IconCopy size={16} />,
                onClick: () => {
                  void copySessionId(s);
                },
              },
            ];
          }
        }
        return (
          <ContextMenu
            open={!!ctxMenu && items.length > 0}
            x={ctxMenu?.x ?? 0}
            y={ctxMenu?.y ?? 0}
            onClose={() => setCtxMenu(null)}
            items={items}
            estimatedHeight={240}
          />
        );
      })()}

      <span hidden data-layout-default={JSON.stringify(DEFAULT_LAYOUT)} />
    </div>
    </ImageViewerProvider>
  );
}

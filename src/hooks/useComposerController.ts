import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type Dispatch,
  type MutableRefObject,
  type RefObject,
  type SetStateAction,
} from "react";
import type { FloatingPos } from "@/lib/floatingMenu";
import type { Locale } from "@/i18n";
import type { AppDialog, Project, SessionContextUsage } from "@/features/app/models";
import type { AcpSessionView, AcpWorkspaceState } from "@/lib/acp/store";
import type { GoalRecordDto } from "@/lib/acp/events";
import type { ChatMessage, SessionSnapshot } from "@/lib/session";
import {
  attachContextWindow,
  formatContextChipLabel,
  type ContextUsageDisplay,
  type ContextUsageSource,
} from "@/lib/contextUsage";
import type {
  DraftNavigationLocation,
  DraftNavigationSnapshot,
} from "@/lib/draftNavigation";
import type { Attachment } from "@/lib/attachments";
import type { PromptHistoryEntry } from "@/lib/composerPromptHistory";
import type { SkillInfo, SlashItem } from "@/lib/slashCatalog";
import type { DragZone } from "@/lib/dragZone";
import { calculateComposerOverlayLayout } from "@/lib/composerOverlayLayout";
import type { ComposerPlusEntry } from "@/components/ComposerPlusPanel";
import { useComposerAttachments } from "./composer/useComposerAttachments";
import { useComposerModes } from "./composer/useComposerModes";
import { useComposerPromptHistory } from "./composer/useComposerPromptHistory";
import { useComposerSlashMenu } from "./composer/useComposerSlashMenu";

export type StateSetter<T> = Dispatch<SetStateAction<T>>;
export type Ref<T> = MutableRefObject<T>;

export interface ComposerPathEntry {
  path: string;
  name: string;
  isDir: boolean;
}

export interface ComposerAttachmentPort {
  pickFiles: () => Promise<string[]>;
  savePastedFile: (name: string, bytes: number[]) => Promise<string>;
  classifyPaths: (paths: string[]) => Promise<ComposerPathEntry[]>;
}

export interface ComposerGoalGetResult {
  /** Goal 存储比较交换修订号。 */
  revision: number;
  /** 当前项目 Goal；修订号为零时缺失。 */
  goal?: GoalRecordDto;
}

export interface ComposerGoalUpsertResult {
  revision: number;
  goal: GoalRecordDto;
}

export interface ComposerGoalClearResult {
  /** 提供项目作用域的 Session 标识。 */
  sessionId: string;
  /** 清理后的修订号。 */
  revision: number;
  /** 已清理 Goal 的墓碑标识。 */
  clearedGoalId: string;
  /** 本次是否命中已完成的相同幂等请求。 */
  deduplicated: boolean;
}

export interface ComposerGoalPort {
  /** 查询当前项目唯一 Goal。 */
  get: (sessionId: string) => Promise<ComposerGoalGetResult>;
  /** 以比较交换语义清理当前项目 Goal。 */
  clear: (args: {
    /** 提供项目作用域的 Session 标识。 */
    sessionId: string;
    /** 当前投影修订号。 */
    expectedRevision: number;
    /** 本次清理的幂等标识。 */
    requestNonce: string;
  }) => Promise<ComposerGoalClearResult>;
  /** 创建或更新当前项目唯一 Goal。 */
  upsert: (args: {
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
      /** 可选人工进度百分比。 */
      progressPercent?: number;
      /** 可选 Token 预算。 */
      tokenBudget?: number;
    };
    /** 当前投影修订号。 */
    expectedRevision: number;
    /** 本次变更的幂等标识。 */
    requestNonce: string;
  }) => Promise<ComposerGoalUpsertResult>;
}

export interface ComposerApiPort {
  isTauri: () => boolean;
  attachments: ComposerAttachmentPort;
  skillsList: (projectPath: string | null) => Promise<{ skills: SkillInfo[] }>;
  goals: ComposerGoalPort;
}

export interface ComposerSessionPort {
  sessionId: string | null;
  state: SessionSnapshot["state"];
  activeProject: Project | null;
  messages: ChatMessage[];
  acpSessionView: AcpSessionView | null;
  contextUsage: SessionContextUsage | null;
  modelId: string;
  modelContextWindow?: number | null;
}

export interface ComposerWorkspacePort {
  acpWorkspaceRef: Ref<AcpWorkspaceState>;
  commitWorkspace: () => void;
  applyViewProjectionRef: Ref<(sessionId: string | null) => void>;
}

export interface ComposerNavigationPort {
  location: () => DraftNavigationLocation;
  snapshotRef: Ref<DraftNavigationSnapshot | null>;
}

export interface ComposerFeedbackPort {
  showToast: (message: string, durationMs?: number) => void;
  setLocalError: StateSetter<string | null>;
  setAppDialog: (dialog: AppDialog) => void;
}

export interface ComposerActionPort {
  newChat: () => void | Promise<void>;
  exportActiveSession: () => void | Promise<void>;
}

export type ComposerPlatform = "mac" | "win" | "other";

export interface ComposerDropPort {
  platform: ComposerPlatform;
  hitZone: (clientX: number, clientY: number) => DragZone;
  setDragZone: StateSetter<DragZone>;
  onProjectPaths: (paths: string[]) => void | Promise<void>;
}

export interface UseComposerControllerOptions {
  locale: Locale;
  session: ComposerSessionPort;
  api: ComposerApiPort;
  workspace: ComposerWorkspacePort;
  navigation: ComposerNavigationPort;
  feedback: ComposerFeedbackPort;
  actions: ComposerActionPort;
  /** 问答卡片位于输入区上方，两者共同决定消息底部留白。 */
  askUserWrapRef?: RefObject<HTMLDivElement | null>;
  askUserKey?: string | number | null;
  drop?: ComposerDropPort;
}

export interface ComposerController {
  draft: string;
  draftRef: Ref<string>;
  setDraft: StateSetter<string>;
  handleDraftChange: (next: string) => void;
  attachments: Attachment[];
  attachmentsRef: Ref<Attachment[]>;
  setAttachments: StateSetter<Attachment[]>;
  attachmentLabels: {
    open: string;
    reveal: string;
    copyPath: string;
    copyImage: string;
    addToComposer: string;
    remove: string;
    viewImage: string;
  };
  addAttachmentsFromPaths: (paths: string[]) => Promise<void>;
  addPastedFiles: (files: File[]) => Promise<void>;
  pickComposerFiles: () => Promise<void>;
  skillsLoading: boolean;
  liveSlash: {
    present: boolean;
    query: string;
    start: number;
    end: number;
  };
  liveSlashRef: Ref<ComposerController["liveSlash"]>;
  slashQuery: { start: number; query: string; end: number } | null;
  composerMenuOpen: boolean;
  showComposerPlus: boolean;
  setShowComposerPlus: StateSetter<boolean>;
  composerPanel: "model" | "reasoning" | null;
  setComposerPanel: StateSetter<"model" | "reasoning" | null>;
  slashActiveIndex: number;
  setSlashActiveIndex: StateSetter<number>;
  slashFilterQuery: string;
  composerMenuEntries: ComposerPlusEntry[];
  composerMenuEntriesRef: Ref<ComposerPlusEntry[]>;
  resolveSlashTitle: (item: SlashItem) => string;
  resolveSlashDescription: (item: SlashItem) => string;
  onSlashQueryChange: (
    query: { start: number; query: string; end: number } | null,
  ) => void;
  closeComposerMenu: () => void;
  applySlashItem: (item: SlashItem) => void;
  promptHistoryIndex: number | null;
  promptHistoryIndexRef: Ref<number | null>;
  setPromptHistoryIndex: StateSetter<number | null>;
  promptHistoryOpen: boolean;
  promptHistoryOpenRef: Ref<boolean>;
  setPromptHistoryOpen: StateSetter<boolean>;
  promptHistoryFilter: string;
  setPromptHistoryFilter: StateSetter<string>;
  promptHistoryActive: number;
  setPromptHistoryActive: StateSetter<number>;
  promptHistoryFocusFilter: boolean;
  setPromptHistoryFocusFilter: StateSetter<boolean>;
  promptHistoryEntries: PromptHistoryEntry[];
  promptHistoryPanelRef: Ref<HTMLDivElement | null>;
  closePromptHistory: () => void;
  openPromptHistory: (options?: {
    focusFilter?: boolean;
    seedDraft?: boolean;
  }) => void;
  applyPromptHistoryEntry: (
    entry: PromptHistoryEntry,
    options?: { close?: boolean; listIndex?: number },
  ) => void;
  composerInputRef: Ref<HTMLDivElement | null>;
  composerShellRef: Ref<HTMLDivElement | null>;
  composerWrapRef: Ref<HTMLDivElement | null>;
  composerPlusTriggerRef: Ref<HTMLButtonElement | null>;
  composerPlusPanelRef: Ref<HTMLDivElement | null>;
  composerPlusPos: FloatingPos | null;
  composerPlusStyle: CSSProperties | undefined;
  promptHistoryPos: FloatingPos | null;
  promptHistoryStyle: CSSProperties | undefined;
  composerFloatPad: number;
  /** 输入区独立高度，用于将问答卡片定位在停止按钮上方。 */
  composerHeight: number;
  requestComposerFocus: () => void;
  syncComposerHeight: () => void;
  contextUsageDisplay: ContextUsageDisplay;
  goalModeSessionKey: string | null;
  setGoalModeSessionKey: StateSetter<string | null>;
  planModeSessionKey: string | null;
  setPlanModeSessionKey: StateSetter<string | null>;
  ultraModeSessionKey: string | null;
  setUltraModeSessionKey: StateSetter<string | null>;
  showStatusModal: boolean;
  setShowStatusModal: StateSetter<boolean>;
  goalToolCompletionSignature: string;
  confirmClearCurrentGoal: () => void;
  editCurrentGoal: () => void;
}

function resizeComposerElement(element: HTMLElement): void {
  const lineHeight = 22;
  const minHeight = lineHeight * 2;
  const maxHeight = lineHeight * 10;
  element.style.height = "auto";
  element.style.height = `${Math.min(
    Math.max(element.scrollHeight, minHeight),
    maxHeight,
  )}px`;
}

/** Composer 的唯一状态边界：草稿、附件、菜单、历史、模式与目标投影。 */
export function useComposerController({
  locale,
  session,
  api,
  workspace,
  navigation,
  feedback,
  actions,
  askUserWrapRef,
  askUserKey = null,
  drop,
}: UseComposerControllerOptions): ComposerController {
  const [draft, setDraftState] = useState("");
  const draftRef = useRef(draft);
  draftRef.current = draft;
  const setDraft = useCallback((update: SetStateAction<string>) => {
    const next =
      typeof update === "function" ? update(draftRef.current) : update;
    draftRef.current = next;
    setDraftState(next);
  }, []);

  const [composerPanel, setComposerPanel] = useState<
    "model" | "reasoning" | null
  >(null);
  const composerInputRef = useRef<HTMLDivElement | null>(null);
  const composerShellRef = useRef<HTMLDivElement | null>(null);
  const composerWrapRef = useRef<HTMLDivElement | null>(null);
  const composerPlusTriggerRef = useRef<HTMLButtonElement | null>(null);
  const composerPlusPanelRef = useRef<HTMLDivElement | null>(null);
  const pendingComposerFocusRef = useRef(false);

  const modes = useComposerModes({
    locale,
    session,
    api,
    workspace,
    feedback,
  });
  const {
    setGoalModeSessionKey,
    setPlanModeSessionKey,
    setShowStatusModal,
  } = modes;
  const actionPortsRef = useRef({ session, actions });
  actionPortsRef.current = { session, actions };
  const handleSlashAction = useCallback(
    (action: string) => {
      const currentPorts = actionPortsRef.current;
      const key = currentPorts.session.sessionId ?? "__draft__";
      switch (action) {
        case "goal":
          if (!currentPorts.session.acpSessionView?.goal.goal) {
            modes.activateGoalMode(key);
            setPlanModeSessionKey(null);
          }
          return;
        case "plan":
          modes.togglePlanMode(key);
          setGoalModeSessionKey(null);
          return;
        case "status":
          setShowStatusModal(true);
          return;
        case "newChat":
          void currentPorts.actions.newChat();
          return;
        case "export":
          void currentPorts.actions.exportActiveSession();
          return;
        default:
          return;
      }
    },
    [modes.activateGoalMode, modes.togglePlanMode],
  );

  const slash = useComposerSlashMenu({
    locale,
    api,
    projectPath: session.activeProject?.path ?? null,
    setDraft,
    onAction: handleSlashAction,
    composerInputRef,
    composerShellRef,
    composerPlusTriggerRef,
    composerPlusPanelRef,
  });
  const attachments = useComposerAttachments({
    locale,
    api,
    navigation,
    feedback,
    closeComposerMenu: slash.closeComposerMenu,
    drop,
  });
  const promptHistory = useComposerPromptHistory({
    locale,
    messages: session.messages,
    setDraft,
    feedback,
    closeComposerMenu: slash.closeComposerMenu,
    composerInputRef,
    composerShellRef,
  });

  const requestComposerFocus = useCallback(() => {
    pendingComposerFocusRef.current = true;
    const tryFocus = (attemptsLeft: number) => {
      const element = composerInputRef.current;
      if (element && element.getAttribute("contenteditable") !== "false") {
        element.focus({ preventScroll: true });
        resizeComposerElement(element);
        try {
          const selection = window.getSelection();
          if (selection) {
            const range = document.createRange();
            range.selectNodeContents(element);
            range.collapse(false);
            selection.removeAllRanges();
            selection.addRange(range);
          }
        } catch {
          // Selection APIs are unavailable in a few WebView lifecycle windows.
        }
        if (document.activeElement === element) {
          pendingComposerFocusRef.current = false;
          return;
        }
      }
      if (attemptsLeft <= 0) {
        pendingComposerFocusRef.current = false;
        return;
      }
      requestAnimationFrame(() => tryFocus(attemptsLeft - 1));
    };
    window.setTimeout(() => tryFocus(12), 0);
  }, []);

  const syncComposerHeight = useCallback(() => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        const element = composerInputRef.current;
        if (element) resizeComposerElement(element);
      });
    });
  }, []);

  const [{ composerFloatPad, composerHeight }, setComposerOverlayLayout] =
    useState({ composerFloatPad: 168, composerHeight: 168 });
  const welcomeSession =
    !session.sessionId &&
    session.messages.length === 0 &&
    session.state !== "streaming";
  useEffect(() => {
    const element = composerWrapRef.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    /** 定位只依赖输入区自身高度，总留白不参与定位，避免测量反馈。 */
    const measure = () => {
      const measured = calculateComposerOverlayLayout(
        element.getBoundingClientRect().height,
        askUserWrapRef?.current?.getBoundingClientRect().height ?? 0,
      );
      if (measured.composerHeight <= 0) return;
      setComposerOverlayLayout((previous) =>
        previous.composerHeight === measured.composerHeight &&
        previous.composerFloatPad === measured.composerFloatPad
          ? previous
          : measured,
      );
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    if (askUserWrapRef?.current) observer.observe(askUserWrapRef.current);
    return () => observer.disconnect();
  }, [
    askUserKey,
    askUserWrapRef,
    attachments.attachments.length,
    slash.composerMenuOpen,
    draft,
    session.messages.length,
    welcomeSession,
  ]);

  const contextUsageDisplay = useMemo<ContextUsageDisplay>(() => {
    const usage = session.contextUsage;
    const source: ContextUsageSource = usage?.estimated ? "estimated" : "known";
    const display = usage
      ? {
          tokens: usage.used,
          source,
          label: formatContextChipLabel(usage.used, source),
        }
      : { tokens: null, source: "unknown" as const, label: "—" };
    return attachContextWindow(
      display,
      usage?.size ?? session.modelContextWindow,
    );
  }, [session.contextUsage, session.modelContextWindow]);

  useEffect(() => {
    if (pendingComposerFocusRef.current) {
      requestComposerFocus();
      return;
    }
    syncComposerHeight();
  }, [draft, requestComposerFocus, session.sessionId, syncComposerHeight]);

  return {
    draft,
    draftRef,
    setDraft,
    handleDraftChange: promptHistory.handleDraftChange,
    ...attachments,
    skillsLoading: slash.skillsLoading,
    liveSlash: slash.liveSlash,
    liveSlashRef: slash.liveSlashRef,
    slashQuery: slash.slashQuery,
    composerMenuOpen: slash.composerMenuOpen,
    showComposerPlus: slash.showComposerPlus,
    setShowComposerPlus: slash.setShowComposerPlus,
    composerPanel,
    setComposerPanel,
    slashActiveIndex: slash.slashActiveIndex,
    setSlashActiveIndex: slash.setSlashActiveIndex,
    slashFilterQuery: slash.slashFilterQuery,
    composerMenuEntries: slash.composerMenuEntries,
    composerMenuEntriesRef: slash.composerMenuEntriesRef,
    resolveSlashTitle: slash.resolveSlashTitle,
    resolveSlashDescription: slash.resolveSlashDescription,
    onSlashQueryChange: slash.onSlashQueryChange,
    closeComposerMenu: slash.closeComposerMenu,
    applySlashItem: slash.applySlashItem,
    promptHistoryIndex: promptHistory.promptHistoryIndex,
    promptHistoryIndexRef: promptHistory.promptHistoryIndexRef,
    setPromptHistoryIndex: promptHistory.setPromptHistoryIndex,
    promptHistoryOpen: promptHistory.promptHistoryOpen,
    promptHistoryOpenRef: promptHistory.promptHistoryOpenRef,
    setPromptHistoryOpen: promptHistory.setPromptHistoryOpen,
    promptHistoryFilter: promptHistory.promptHistoryFilter,
    setPromptHistoryFilter: promptHistory.setPromptHistoryFilter,
    promptHistoryActive: promptHistory.promptHistoryActive,
    setPromptHistoryActive: promptHistory.setPromptHistoryActive,
    promptHistoryFocusFilter: promptHistory.promptHistoryFocusFilter,
    setPromptHistoryFocusFilter: promptHistory.setPromptHistoryFocusFilter,
    promptHistoryEntries: promptHistory.promptHistoryEntries,
    promptHistoryPanelRef: promptHistory.promptHistoryPanelRef,
    closePromptHistory: promptHistory.closePromptHistory,
    openPromptHistory: promptHistory.openPromptHistory,
    applyPromptHistoryEntry: promptHistory.applyPromptHistoryEntry,
    composerInputRef,
    composerShellRef,
    composerWrapRef,
    composerPlusTriggerRef,
    composerPlusPanelRef,
    composerPlusPos: slash.composerPlusPos,
    composerPlusStyle: slash.composerPlusStyle,
    promptHistoryPos: promptHistory.promptHistoryPos,
    promptHistoryStyle: promptHistory.promptHistoryStyle,
    composerFloatPad,
    composerHeight,
    requestComposerFocus,
    syncComposerHeight,
    contextUsageDisplay,
    goalModeSessionKey: modes.goalModeSessionKey,
    setGoalModeSessionKey: modes.setGoalModeSessionKey,
    planModeSessionKey: modes.planModeSessionKey,
    setPlanModeSessionKey: modes.setPlanModeSessionKey,
    ultraModeSessionKey: modes.ultraModeSessionKey,
    setUltraModeSessionKey: modes.setUltraModeSessionKey,
    showStatusModal: modes.showStatusModal,
    setShowStatusModal: modes.setShowStatusModal,
    goalToolCompletionSignature: modes.goalToolCompletionSignature,
    confirmClearCurrentGoal: modes.confirmClearCurrentGoal,
    editCurrentGoal: modes.editCurrentGoal,
  };
}

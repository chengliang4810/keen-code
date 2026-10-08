import { selectedReasoningLevel } from "@/modules/ai/lib/reasoning";
import {
  effectiveCustomModel,
  resolveEndpointModel,
} from "@/modules/ai/config";
import { getDefaultChatDirectory } from "@/modules/ai/lib/defaultChatDirectory";
import {
  normalizePermissionMode,
  type PermissionMode,
} from "@/modules/ai/lib/permissions";
import { currentWorkspaceScopeKey } from "@/modules/workspace";
import type { Chat, UIMessage } from "@ai-sdk/react";
import { create } from "zustand";
import {
  endpointIdFromCompatModel,
  isCompatModelId,
  type ProviderId,
} from "../config";
import {
  NO_MODEL_ID,
  isConfiguredCustomModel,
} from "@/modules/ai/lib/modelSelection";
import { usePreferencesStore } from "@/modules/settings/preferences";
import { useTodosStore } from "./todoStore";
import { usePlanStore } from "./planStore";
import { isTaskRunning, resolveTaskWorkspace } from "../lib/taskWorkspace";
import type { AgentUsage } from "../lib/agent";
import {
  EMPTY_PROVIDER_KEYS,
  type ProviderKeys,
  type CustomEndpointKeys,
} from "../lib/keyring";
import {
  deleteSessionData,
  deleteArchivedSessionData,
  deriveTitle,
  loadAll,
  loadMessages,
  newSessionId,
  saveActiveId,
  saveMessages,
  saveSessionsList,
  saveSessionsListAndFlush,
  type SessionMeta,
} from "../lib/sessions";
import { pushRecentModel } from "../lib/modelPrefs";
import {
  changeArchivedConversations,
  type ArchiveOperation,
} from "@/modules/ai/lib/archivedConversations";

export type Live = {
  getCwd: () => string | null;
  getTerminalContext: () => string | null;
  isActiveTerminalPrivate: () => boolean;
  injectIntoActivePty: (text: string) => boolean;
  getWorkspaceRoot: () => string | null;
  getActiveFile: () => string | null;
  openPreview: (url: string) => boolean;
  spawnManagedAgent: (
    prompt: string,
    sessionId: string,
  ) => { tabId: number; leafId: number } | null;
  readLeafBuffer: (leafId: number) => string | null;
};

export type AgentRunStatus =
  | "idle"
  | "thinking"
  | "streaming"
  | "awaiting-approval"
  | "error";

export type AgentMeta = {
  status: AgentRunStatus;
  step: string | null;
  approvalsPending: number;
  error: string | null;
  tokens: AgentUsage;
  lastInputTokens: number;
  lastCachedTokens: number;
  hitStepCap: boolean;
  compactionNotice: { droppedCount: number; at: number } | null;
};

const ZERO_USAGE: AgentUsage = {
  inputTokens: 0,
  outputTokens: 0,
  cachedInputTokens: 0,
};

const IDLE_META: AgentMeta = {
  status: "idle",
  step: null,
  approvalsPending: 0,
  error: null,
  tokens: ZERO_USAGE,
  lastInputTokens: 0,
  lastCachedTokens: 0,
  hitStepCap: false,
  compactionNotice: null,
};

export type MiniState = {
  open: boolean;
};

export type PendingSelection = {
  id: string;
  text: string;
  source: "terminal" | "editor";
};

export type ApprovalResponder = (approvalId: string, approved: boolean) => void;

type StoreState = {
  live: Live;
  setLive: (live: Live) => void;

  /**
   * Set by AgentRunBridge each render. Lets surfaces outside the chat hook
   * tree (e.g. the AI diff tab in the editor area) resolve a pending tool
   * approval through the active session's `addToolApprovalResponse`.
   */
  approvalResponder: ApprovalResponder | null;
  setApprovalResponder: (fn: ApprovalResponder | null) => void;
  respondToApproval: (approvalId: string, approved: boolean) => void;

  apiKeys: ProviderKeys;
  setApiKeys: (keys: ProviderKeys) => void;
  setApiKey: (provider: ProviderId, key: string | null) => void;

  customEndpointKeys: CustomEndpointKeys;
  setCustomEndpointKeys: (keys: CustomEndpointKeys) => void;

  selectedModelId: string;
  setSessionReasoningLevel: (
    sessionId: string,
    modelId: string,
    level: string,
  ) => void;
  setSelectedModelId: (id: string) => void;

  mini: MiniState;
  openMini: () => void;
  closeMini: () => void;
  toggleMini: () => void;

  panelOpen: boolean;
  openPanel: () => void;
  closePanel: () => void;
  togglePanel: () => void;

  focusSignal: number;
  pendingPrefill: string | null;
  focusInput: (prefill?: string | null) => void;
  consumePrefill: () => string | null;

  pendingSelections: PendingSelection[];
  attachSelection: (text: string, source: "terminal" | "editor") => void;
  consumeSelections: () => PendingSelection[];

  agentMeta: AgentMeta;
  patchAgentMeta: (patch: Partial<AgentMeta>) => void;
  resetAgentMeta: () => void;

  // Sessions
  sessionsHydrated: boolean;
  sessionsLoading: boolean;
  sessionsError: string | null;
  sessions: SessionMeta[];
  /** 草稿持有内存元数据；首条消息或携带工具切换项目时加入 sessions。 */
  draftSession: SessionMeta | null;
  activeSessionId: string | null;
  sessionLoading: boolean;
  /** 懒加载发送链路期间锁定导航，避免首条消息被提交到已经离开的草稿。 */
  sessionSubmitting: boolean;
  hydrateSessions: () => Promise<void>;
  /** null 创建独立草稿；携带工具切换归属时保留原 ID 并创建新草稿。 */
  newSession: (
    project?: {
      id: string;
      root: string | null;
      scope: string;
    } | null,
    preserveDraftWorkspace?: boolean,
  ) => string;
  switchSession: (id: string) => Promise<boolean>;
  deleteSession: (id: string) => void;
  renameSession: (id: string, title: string) => void;
  setSessionPermissionMode: (id: string, mode: PermissionMode) => void;
  bindSessionWorkspace: (id: string, root: string, scope: string) => void;
  archiveSession: (id: string, archived: boolean) => void;
  assignSessionProject: (id: string, projectId: string) => void;
  /** Persist messages of a session and bump its updatedAt + auto-title. */
  persistMessages: (id: string, messages: UIMessage[]) => void;
  recordSessionActivity: (id: string) => void;
};

const NOOP_LIVE: Live = {
  getCwd: () => null,
  getTerminalContext: () => null,
  isActiveTerminalPrivate: () => false,
  injectIntoActivePty: () => false,
  getWorkspaceRoot: () => null,
  getActiveFile: () => null,
  openPreview: () => false,
  spawnManagedAgent: () => null,
  readLeafBuffer: () => null,
};

const CHATS_LRU_CAP = 8;
export const chats = new Map<string, Chat<UIMessage>>();

export function touchChat(id: string, c: Chat<UIMessage>) {
  if (chats.has(id)) chats.delete(id);
  chats.set(id, c);
  while (chats.size > CHATS_LRU_CAP) {
    const oldest = chats.keys().next().value;
    if (!oldest || oldest === id) break;
    if (useChatStore.getState().activeSessionId === oldest) break;
    flushPersistEntry(oldest);
    void chats.get(oldest)?.stop();
    chats.delete(oldest);
  }
}
// Initial messages for a session, populated at hydration time and consumed
// when the matching Chat is constructed.
export const seedMessages = new Map<string, UIMessage[]>();

// Trailing debounce for per-token message persistence. Streaming fires
// `persistMessages` on every token; without this we'd JSON-serialize the
// full message array and round-trip to the store plugin per token, which
// stalls the UI. Flush on idle (status transition) via `flushPersist`.
const PERSIST_DEBOUNCE_MS = 300;
const pendingPersist = new Map<
  string,
  { latest: UIMessage[]; timer: ReturnType<typeof setTimeout> }
>();
let hydrationFlight: Promise<void> | null = null;

function releaseSessionTools(id: string): void {
  void import("@/modules/ai/tools/shell")
    .then(({ releaseSessionShells }) => releaseSessionShells(id))
    .catch((error) =>
      console.error("[rcode] session shell cleanup failed", error),
    );
}

function flushPersistEntry(id: string) {
  const entry = pendingPersist.get(id);
  if (!entry) return;
  clearTimeout(entry.timer);
  pendingPersist.delete(id);
  void saveMessages(id, entry.latest);
}

export function flushPersist(id?: string): void {
  if (id) {
    flushPersistEntry(id);
    return;
  }
  for (const key of Array.from(pendingPersist.keys())) flushPersistEntry(key);
}

export const useChatStore = create<StoreState>((set, get) => ({
  live: NOOP_LIVE,
  setLive: (live) => set({ live }),

  approvalResponder: null,
  setApprovalResponder: (fn) => set({ approvalResponder: fn }),
  respondToApproval: (approvalId, approved) => {
    const fn = get().approvalResponder;
    if (fn) fn(approvalId, approved);
  },

  apiKeys: { ...EMPTY_PROVIDER_KEYS },
  setApiKeys: (keys) => set({ apiKeys: keys }),
  setApiKey: (provider, key) => {
    set({ apiKeys: { ...get().apiKeys, [provider]: key } });
  },

  customEndpointKeys: {},
  setCustomEndpointKeys: (keys) => set({ customEndpointKeys: keys }),

  selectedModelId: NO_MODEL_ID,
  setSelectedModelId: (id) => {
    if (id === get().selectedModelId) return;
    set({ selectedModelId: id });
    const model = resolveEndpointModel(
      id,
      usePreferencesStore.getState().customEndpoints,
    );
    const level = model
      ? selectedReasoningLevel(
          id,
          effectiveCustomModel(model.model, model.endpoint.baseURL)
            .reasoningLevels,
        )
      : undefined;
    const sessionId = get().activeSessionId;
    if (level && sessionId)
      get().setSessionReasoningLevel(sessionId, id, level);
    if (id !== NO_MODEL_ID) void pushRecentModel(id);
  },

  mini: { open: false },
  openMini: () => set({ mini: { open: true } }),
  closeMini: () => set({ mini: { open: false } }),
  toggleMini: () => set((s) => ({ mini: { open: !s.mini.open } })),

  panelOpen: true,
  openPanel: () => set({ panelOpen: true }),
  closePanel: () => set({ panelOpen: false }),
  togglePanel: () => set((s) => ({ panelOpen: !s.panelOpen })),

  focusSignal: 0,
  pendingPrefill: null,
  focusInput: (prefill = null) =>
    set((s) => ({
      panelOpen: true,
      focusSignal: s.focusSignal + 1,
      pendingPrefill: prefill ?? null,
    })),
  consumePrefill: () => {
    const v = get().pendingPrefill;
    if (v != null) set({ pendingPrefill: null });
    return v;
  },

  pendingSelections: [],
  attachSelection: (text, source) => {
    const trimmed = text.trim();
    if (!trimmed) return;
    const id = `sel-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    set((s) => ({
      panelOpen: true,
      focusSignal: s.focusSignal + 1,
      pendingSelections: [
        ...s.pendingSelections,
        { id, text: trimmed, source },
      ],
    }));
  },
  consumeSelections: () => {
    const v = get().pendingSelections;
    if (v.length > 0) set({ pendingSelections: [] });
    return v;
  },

  agentMeta: IDLE_META,
  patchAgentMeta: (patch) =>
    set((s) => ({ agentMeta: { ...s.agentMeta, ...patch } })),
  resetAgentMeta: () => set({ agentMeta: IDLE_META }),

  sessionsHydrated: false,
  sessionsLoading: false,
  sessionsError: null,
  sessions: [],
  draftSession: null,
  activeSessionId: null,
  sessionLoading: false,
  sessionSubmitting: false,

  hydrateSessions: () => {
    if (get().sessionsHydrated) return Promise.resolve();
    if (hydrationFlight) return hydrationFlight;
    const retry = get().sessionsError !== null;
    set({ sessionsLoading: true, sessionsError: null });
    hydrationFlight = (async () => {
      try {
        const { sessions, activeId } = await loadAll(retry);
        const restored =
          sessions.find((s) => s.id === activeId && !s.archived) ??
          sessions.find((s) => !s.archived);
        if (restored) {
          const messages = await loadMessages(restored.id);
          if (messages?.length) seedMessages.set(restored.id, messages);
          set({
            sessions,
            activeSessionId: restored.id,
            sessionsHydrated: true,
            sessionsLoading: false,
          });
          return;
        }
        set({ sessions, sessionsHydrated: true, sessionsLoading: false });
        get().newSession();
      } catch (error) {
        set({
          sessionsError: error instanceof Error ? error.message : String(error),
          sessionsLoading: false,
        });
      } finally {
        hydrationFlight = null;
      }
    })();
    return hydrationFlight;
  },

  newSession: (project, preserveDraftWorkspace = false) => {
    if (isSessionNavigationLocked()) return get().activeSessionId ?? "";
    sessionSwitchRequest++;
    const existingDraft = get().draftSession;
    // 无工具时复用草稿，已有工具的归属不能随项目选择一起移动。
    const current =
      get().sessions.find((s) => s.id === get().activeSessionId) ??
      existingDraft;
    const binding =
      project !== undefined
        ? project
        : current?.projectId
          ? {
              id: current.projectId,
              root: current.workspaceRoot,
              scope: current.workspaceScope,
            }
          : current?.projectless
            ? null
            : undefined;
    const changedOwner =
      existingDraft &&
      (existingDraft.projectId !== binding?.id ||
        !!existingDraft.projectless !== (binding === null));
    const preserve = preserveDraftWorkspace && changedOwner;
    if (preserve) {
      const sessions = [...get().sessions, existingDraft];
      set({ sessions });
      void saveSessionsList(sessions);
    }
    const id = !preserve && existingDraft ? existingDraft.id : newSessionId();
    const meta: SessionMeta = {
      id,
      title: "New chat",
      permissionMode:
        !preserve && existingDraft ? existingDraft.permissionMode : undefined,
      createdAt:
        !preserve && existingDraft ? existingDraft.createdAt : Date.now(),
      updatedAt: Date.now(),
      ...(binding
        ? {
            projectId: binding.id,
            workspaceRoot: binding.root ?? undefined,
            workspaceScope: binding.scope,
          }
        : binding === null
          ? {
              projectless: true,
              workspaceScope: "local",
              workspaceRoot: getDefaultChatDirectory() ?? undefined,
            }
          : {}),
    };
    set({
      draftSession: meta,
      activeSessionId: id,
      agentMeta: IDLE_META,
      sessionLoading: false,
    });
    // 草稿 ID 不落盘，重启时仍恢复正式历史或进入新的起始页。
    return id;
  },

  switchSession: async (id) => {
    if (isSessionNavigationLocked()) return false;
    const request = ++sessionSwitchRequest;
    if (get().activeSessionId === id) {
      set({ sessionLoading: false });
      return true;
    }
    if (get().draftSession?.id === id) {
      set({ activeSessionId: id, agentMeta: IDLE_META, sessionLoading: false });
      return true;
    }
    if (!get().sessions.some((s) => s.id === id && !s.archived)) {
      set({ sessionLoading: false });
      return false;
    }

    // Lazily seed the chat with persisted messages the first time we open
    // this session. Subsequent switches reuse the cached Chat instance.
    const flip = () => {
      set({ activeSessionId: id, agentMeta: IDLE_META, sessionLoading: false });
      void saveActiveId(id);
    };
    if (chats.has(id) || seedMessages.has(id)) {
      flip();
      return true;
    }
    set({ sessionLoading: true });
    try {
      const m = await loadMessages(id);
      // 快速点击多个任务时，只应用最后一次导航意图。
      if (request !== sessionSwitchRequest || isSessionNavigationLocked())
        return false;
      if (!get().sessions.some((s) => s.id === id && !s.archived)) return false;
      if (m && m.length > 0 && !chats.has(id)) seedMessages.set(id, m);
      flip();
      return true;
    } catch {
      if (request === sessionSwitchRequest) set({ sessionLoading: false });
      return false;
    }
  },

  deleteSession: (id) => {
    if (isSessionNavigationLocked()) return;
    sessionSwitchRequest++;
    const current = get().sessions.find((s) => s.id === id);
    const remaining = get().sessions.filter((s) => s.id !== id);
    chats.get(id)?.stop();
    chats.delete(id);
    seedMessages.delete(id);
    releaseSessionTools(id);
    const pend = pendingPersist.get(id);
    if (pend) {
      clearTimeout(pend.timer);
      pendingPersist.delete(id);
    }
    void deleteSessionData(id);
    void useTodosStore.getState().clearSession(id);

    const wasActive = get().activeSessionId === id;
    set({ sessions: remaining, sessionLoading: false });
    void saveSessionsList(remaining);
    if (wasActive) {
      const replacement = remaining.find(
        (s) => !s.archived && s.projectId === current?.projectId,
      );
      if (replacement) get().switchSession(replacement.id);
      else
        get().newSession(
          current?.projectId
            ? {
                id: current.projectId,
                root: current.workspaceRoot ?? null,
                scope: current.workspaceScope ?? currentWorkspaceScopeKey(),
              }
            : current?.projectless
              ? null
              : undefined,
        );
    }
  },

  renameSession: (id, title) => {
    if (get().sessionsError || get().sessionsLoading) return;
    const next = get().sessions.map((s) =>
      s.id === id ? { ...s, title, updatedAt: Date.now() } : s,
    );
    set({ sessions: next });
    void saveSessionsList(next);
  },

  setSessionReasoningLevel: (id, modelId, level) => {
    if (isSessionNavigationLocked()) return;
    const model = resolveEndpointModel(
      modelId,
      usePreferencesStore.getState().customEndpoints,
    );
    if (
      !model ||
      !effectiveCustomModel(
        model.model,
        model.endpoint.baseURL,
      ).reasoningLevels.includes(level)
    )
      return;
    const reasoningSelection = { modelId, level };
    const draft = get().draftSession;
    if (draft?.id === id) {
      set({ draftSession: { ...draft, reasoningSelection } });
      return;
    }
    if (!get().sessions.some((session) => session.id === id)) return;
    const sessions = get().sessions.map((session) =>
      session.id === id ? { ...session, reasoningSelection } : session,
    );
    set({ sessions });
    void saveSessionsList(sessions);
  },

  setSessionPermissionMode: (id, mode) => {
    if (isSessionNavigationLocked()) return;
    const permissionMode = normalizePermissionMode(mode);
    const draft = get().draftSession;
    if (draft?.id === id) {
      set({ draftSession: { ...draft, permissionMode } });
      return;
    }
    if (!get().sessions.some((session) => session.id === id)) return;
    const sessions = get().sessions.map((session) =>
      session.id === id ? { ...session, permissionMode } : session,
    );
    set({ sessions });
    void saveSessionsList(sessions);
  },

  bindSessionWorkspace: (id, root, scope) => {
    if (get().sessionsError || get().sessionsLoading) return;
    const draft = get().draftSession;
    if (draft?.id === id) {
      if (
        !draft.workspaceRoot &&
        (!draft.projectless ||
          (scope === "local" && root === getDefaultChatDirectory()))
      )
        set({
          draftSession: {
            ...draft,
            workspaceRoot: root,
            workspaceScope: scope,
          },
        });
      return;
    }
    const meta = get().sessions.find((s) => s.id === id);
    if (
      !meta ||
      meta.workspaceRoot ||
      (meta.projectless &&
        (scope !== "local" || root !== getDefaultChatDirectory()))
    )
      return;
    const next = get().sessions.map((s) =>
      s.id === id ? { ...s, workspaceRoot: root, workspaceScope: scope } : s,
    );
    set({ sessions: next });
    void saveSessionsList(next);
  },

  archiveSession: (id, archived) => {
    if (isSessionNavigationLocked()) return;
    const next = get().sessions.map((s) =>
      s.id === id ? { ...s, archived } : s,
    );
    set({ sessions: next });
    void saveSessionsList(next);
    if (archived && get().activeSessionId === id) {
      const current = next.find((s) => s.id === id);
      // 归档当前任务后留在同一项目，避免绕过运行环境导航。
      const remaining = next.find(
        (s) => !s.archived && s.projectId === current?.projectId,
      );
      if (remaining) get().switchSession(remaining.id);
      else get().newSession();
    }
  },

  assignSessionProject: (id, projectId) => {
    if (get().sessionsError || get().sessionsLoading) return;
    const draft = get().draftSession;
    if (draft?.id === id) {
      set({ draftSession: { ...draft, projectId } });
      return;
    }
    const next = get().sessions.map((s) =>
      s.id === id ? { ...s, projectId } : s,
    );
    set({ sessions: next });
    void saveSessionsList(next);
  },

  persistMessages: (id, messages) => {
    if (get().sessionsError || get().sessionsLoading) return;
    const draft = get().draftSession;
    if (draft?.id === id) {
      const submitted = messages.some(
        (message) =>
          message.role === "user" &&
          message.parts.some(
            (part) =>
              (part.type === "text" && part.text.trim().length > 0) ||
              part.type === "file",
          ),
      );
      if (!submitted) return;
      const now = Date.now();
      const meta = {
        ...draft,
        title: deriveTitle(messages),
        createdAt: now,
        updatedAt: now,
      };
      const sessions = [meta, ...get().sessions];
      set({ sessions, draftSession: null });
      void saveSessionsList(sessions);
      if (get().activeSessionId === id) void saveActiveId(id);
    }
    // 不保存尚未提交的草稿或已经删除会话的延迟回调。
    if (!get().sessions.some((session) => session.id === id)) return;
    // Debounce the message-blob write so streaming doesn't pound the store.
    const existing = pendingPersist.get(id);
    if (existing) clearTimeout(existing.timer);
    const timer = setTimeout(() => {
      const entry = pendingPersist.get(id);
      if (!entry) return;
      pendingPersist.delete(id);
      void saveMessages(id, entry.latest);
    }, PERSIST_DEBOUNCE_MS);
    pendingPersist.set(id, { latest: messages, timer });

    // Update zustand session list only when the derived title actually
    // changes — otherwise we'd rewrite the sessions array (and trigger
    // re-renders + a store write) on every token.
    const sessions = get().sessions;
    const meta = sessions.find((s) => s.id === id);
    if (!meta) return;
    const isUntitled = !meta.title || meta.title === "New chat";
    if (!isUntitled) return;
    const nextTitle = deriveTitle(messages);
    if (nextTitle === meta.title) return;
    const next = sessions.map((s) =>
      s.id === id ? { ...s, title: nextTitle, updatedAt: Date.now() } : s,
    );
    set({ sessions: next });
    void saveSessionsList(next);
  },

  recordSessionActivity: (id) => {
    if (get().sessionsError || get().sessionsLoading) return;
    if (!get().sessions.some((session) => session.id === id)) return;
    const sessions = get().sessions.map((session) =>
      session.id === id ? { ...session, updatedAt: Date.now() } : session,
    );
    set({ sessions });
    void saveSessionsList(sessions);
  },
}));

export function getAgentMeta(): AgentMeta {
  return useChatStore.getState().agentMeta;
}

let sessionSwitchRequest = 0;
let archiveManagementBusy = false;

/** 归档管理在主窗口串行执行，保存成功前不更新 UI，也不切换当前会话。 */
export async function manageArchivedSessions(
  operation: ArchiveOperation,
  ids: string[],
): Promise<void> {
  if (!useChatStore.getState().sessionsHydrated)
    throw new Error("Conversations are still loading. Try again.");
  if (isSessionNavigationLocked() || useChatStore.getState().sessionLoading)
    throw new Error(
      "Finish the current agent operation before managing archived conversations.",
    );
  archiveManagementBusy = true;
  try {
    for (;;) {
      const prev = useChatStore.getState();
      if (ids.includes(prev.activeSessionId ?? ""))
        throw new Error(
          "The current conversation cannot be managed from archives.",
        );
      const next = changeArchivedConversations(prev.sessions, ids, operation);
      try {
        await saveSessionsListAndFlush(next);
      } catch (error) {
        // 显式落盘失败也要还原插件内存，否则后续自动保存会写入未确认的变更。
        await saveSessionsList(useChatStore.getState().sessions);
        throw error;
      }
      if (useChatStore.getState().sessions !== prev.sessions) continue;
      useChatStore.setState({ sessions: next });
      break;
    }
    if (operation === "delete") {
      for (const id of ids) {
        chats.get(id)?.stop();
        chats.delete(id);
        seedMessages.delete(id);
        releaseSessionTools(id);
        const pending = pendingPersist.get(id);
        if (pending) clearTimeout(pending.timer);
        pendingPersist.delete(id);
      }
      await deleteArchivedSessionData(ids);
      await Promise.all(
        ids.map((id) => useTodosStore.getState().clearSession(id)),
      );
    }
  } finally {
    archiveManagementBusy = false;
  }
}

/** 当前执行桥只有一个状态所有者，审批和计划未处理时不能切走或删除任务。 */
export function isSessionNavigationLocked(): boolean {
  const state = useChatStore.getState();
  const chat = state.activeSessionId
    ? chats.get(state.activeSessionId)
    : undefined;
  return (
    archiveManagementBusy ||
    state.sessionsLoading ||
    state.sessionsError !== null ||
    state.sessionSubmitting ||
    isTaskRunning(state.agentMeta.status) ||
    chat?.status === "submitted" ||
    chat?.status === "streaming" ||
    usePlanStore.getState().queue.length > 0
  );
}

export function getTaskWorkspace(sessionId: string): string | null {
  const state = useChatStore.getState();
  return resolveTaskWorkspace(
    state.sessions.find((s) => s.id === sessionId) ??
      (state.draftSession?.id === sessionId ? state.draftSession : undefined),
    currentWorkspaceScopeKey(),
    state.live.getWorkspaceRoot(),
    getDefaultChatDirectory(),
  );
}

export function getActiveProviderKey(): string | null {
  const { selectedModelId, customEndpointKeys } = useChatStore.getState();
  if (isCompatModelId(selectedModelId)) {
    const eid = endpointIdFromCompatModel(selectedModelId);
    return customEndpointKeys[eid] ?? null;
  }
  return null;
}

export function hasKeyForModel(modelId: string): boolean {
  return isConfiguredCustomModel(
    modelId,
    usePreferencesStore.getState().customEndpoints,
  );
}

export function getChat(sessionId?: string): Chat<UIMessage> | undefined {
  if (sessionId) return chats.get(sessionId);
  const id = useChatStore.getState().activeSessionId;
  return id ? chats.get(id) : undefined;
}

export function stop(): void {
  const id = useChatStore.getState().activeSessionId;
  if (!id) return;
  void chats.get(id)?.stop();
}

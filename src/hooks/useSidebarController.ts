import { useMemo, useState, type MutableRefObject, type RefObject } from "react";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import type { ResourceOpenTarget } from "@/components/ResourceViewer";
import type {
  AppDialog,
  ContextMenuState,
  Project,
  SessionRow,
} from "@/features/app/models";
import type { SessionSnapshot } from "@/lib/session";
import type { LayoutPrefs } from "@/lib/layout";
import { useSidebarAutoArchive } from "./sidebar/useSidebarAutoArchive";
import { useSidebarActions } from "./sidebar/useSidebarActions";
import { useSidebarDrag } from "./sidebar/useSidebarDrag";
import { useSidebarLists } from "./sidebar/useSidebarLists";
import { useSidebarMenus } from "./sidebar/useSidebarMenus";
import { useSidebarSearch } from "./sidebar/useSidebarSearch";
import { useSidebarTitles } from "./sidebar/useSidebarTitles";
import type {
  SidebarDragKind,
  SidebarDropHint,
  SidebarNewChat,
  SidebarOpenSession,
  SidebarSetState,
  SidebarShowToast,
} from "./sidebar/types";
import type { SessionSearchHits } from "@/lib/sessionSearch";

export type {
  SidebarDragKind,
  SidebarDropHint,
  SidebarNewChat,
  SidebarOpenSession,
  SidebarSetState,
  SidebarShowToast,
} from "./sidebar/types";

export interface UseSidebarControllerOptions {
  locale: Locale;
  /** The session currently shown by the workbench. */
  currentSessionId: string | null;
  activeProject: Project | null;
  setActiveProject: SidebarSetState<Project | null>;
  setSession: SidebarSetState<SessionSnapshot>;
  setAppDialog: SidebarSetState<AppDialog>;
  setLocalError: SidebarSetState<string | null>;
  setLayout: SidebarSetState<LayoutPrefs>;
  setResourceOpenTarget: SidebarSetState<ResourceOpenTarget | null>;
  viewingSessionIdRef: MutableRefObject<string | null>;
  /** Called before/while the initial list request is loading. */
  setAppBooting: SidebarSetState<boolean>;
  /** Reset ACP/UI state when the active project's directory changes. */
  onActiveProjectRelocated?: (project: Project) => void;
  /** Clear the current conversation when its project is removed from the app. */
  onActiveProjectRemoved?: (project: Project) => void;
  newChat: SidebarNewChat;
  openSession: SidebarOpenSession;
  showToast: SidebarShowToast;
  /** Composer fallback focus target when the sidebar search closes. */
  composerInputRef?: RefObject<HTMLElement | null>;
  autoArchiveConversations: boolean;
  archiveRetentionDays: number;
}

export interface UseSidebarControllerResult {
  projects: Project[];
  setProjects: SidebarSetState<Project[]>;
  sessions: SessionRow[];
  setSessions: SidebarSetState<SessionRow[]>;
  sessionsRef: MutableRefObject<SessionRow[]>;
  sessionTitleOverridesRef: MutableRefObject<Map<string, string>>;

  expandedProjects: Record<string, boolean>;
  setExpandedProjects: SidebarSetState<Record<string, boolean>>;
  visibleSessionsByProject: Record<string, number>;
  setVisibleSessionsByProject: SidebarSetState<Record<string, number>>;
  sessionOrder: string[];
  setSessionOrder: SidebarSetState<string[]>;
  projectDropHint: SidebarDropHint | null;
  setProjectDropHint: SidebarSetState<SidebarDropHint | null>;
  projectsOpen: boolean;
  setProjectsOpen: SidebarSetState<boolean>;
  pinnedOpen: boolean;
  setPinnedOpen: SidebarSetState<boolean>;
  historyOpen: boolean;
  setHistoryOpen: SidebarSetState<boolean>;
  ctxMenu: ContextMenuState;
  setCtxMenu: SidebarSetState<ContextMenuState>;
  showSearch: boolean;
  setShowSearch: SidebarSetState<boolean>;
  searchQuery: string;
  setSearchQuery: SidebarSetState<string>;
  searchHits: SessionSearchHits;
  searchTriggerRef: RefObject<HTMLButtonElement | null>;
  searchReturnFocusRef: MutableRefObject<HTMLElement | null>;

  refreshLists: () => Promise<void>;
  refreshSessions: () => Promise<void>;
  refreshProjects: () => Promise<void>;
  sessionsForProject: (projectId: string) => SessionRow[];
  pinnedSessions: SessionRow[];
  orphanSessions: SessionRow[];
  startSidebarDrag: (
    event: React.DragEvent<HTMLElement>,
    kind: SidebarDragKind,
    id: string,
  ) => void;
  endSidebarDrag: () => void;
  dragOverProject: (
    event: React.DragEvent<HTMLElement>,
    targetId: string,
  ) => void;
  dropProject: (
    event: React.DragEvent<HTMLElement>,
    targetId: string,
  ) => void;
  dropSession: (
    event: React.DragEvent<HTMLElement>,
    targetId: string,
  ) => void;
  applyProjectOrder: (ids: string[]) => void;
  openSearch: () => void;
  openSessionMenu: (
    event: React.MouseEvent,
    session: SessionRow,
  ) => void;
  openProjectMenu: (
    event: React.MouseEvent,
    project: Project,
  ) => void;
  renameProject: (project: Project) => void;
  renameSession: (session: SessionRow) => void;
  relocateProject: (project: Project) => Promise<void>;
  removeProjectFromApp: (project: Project) => void;
  archiveSession: (session: SessionRow, archived?: boolean) => Promise<void>;
  pinSession: (session: SessionRow, pinned?: boolean) => Promise<void>;
  copySessionId: (session: SessionRow) => Promise<void>;
  viewTrajectory: (session: SessionRow) => void;
  applyMessagePrefixTitle: (sessionId: string, userText: string) => void;
  applyAutomaticSessionTitle: (
    sessionId: string,
    firstUserMessage: string,
    expectedTitle?: string | null,
  ) => Promise<void>;
  openSession: SidebarOpenSession;
  newChat: SidebarNewChat;
}

/** 管理侧栏树、搜索、项目/会话菜单以及会话标题持久化。ACP 连接与消息投影由调用方负责。 */
export function useSidebarController({
  locale,
  currentSessionId,
  activeProject,
  setActiveProject,
  setSession,
  setAppDialog,
  setLocalError,
  setLayout,
  setResourceOpenTarget,
  viewingSessionIdRef,
  setAppBooting,
  onActiveProjectRelocated,
  onActiveProjectRemoved,
  newChat,
  openSession,
  showToast,
  composerInputRef,
  autoArchiveConversations,
  archiveRetentionDays,
}: UseSidebarControllerOptions): UseSidebarControllerResult {
  const tr = useMemo(() => createT(locale), [locale]);
  const lists = useSidebarLists({
    setActiveProject,
    setAppBooting,
    setLocalError,
  });
  const {
    projects,
    setProjects,
    sessions,
    setSessions,
    sessionsRef,
    expandedProjects,
    setExpandedProjects,
    visibleSessionsByProject,
    setVisibleSessionsByProject,
    sessionOrder,
    setSessionOrder,
    refreshLists,
    refreshSessions,
    refreshProjects,
    sessionsForProject,
    pinnedSessions,
    orphanSessions,
  } = lists;

  useSidebarAutoArchive({
    sessions,
    setSessions,
    autoArchiveConversations,
    archiveRetentionDays,
  });

  const search = useSidebarSearch({
    projects,
    sessions,
    composerInputRef,
  });
  const menus = useSidebarMenus();
  const drag = useSidebarDrag({
    locale,
    projects,
    setProjects,
    sessions,
    sessionOrder,
    setSessionOrder,
    refreshProjects,
    setLocalError,
  });
  const titles = useSidebarTitles({
    tr,
    sessionsRef,
    setSessions,
    setSession,
  });
  const actions = useSidebarActions({
    locale,
    tr,
    activeProject,
    projects,
    currentSessionId,
    viewingSessionIdRef,
    setActiveProject,
    setAppDialog,
    setLocalError,
    setLayout,
    setResourceOpenTarget,
    setVisibleSessionsByProject,
    setExpandedProjects,
    setCtxMenu: menus.setCtxMenu,
    onActiveProjectRelocated,
    onActiveProjectRemoved,
    newChat,
    showToast,
    refreshProjects,
    refreshSessions,
    applySessionTitle: titles.applySessionTitle,
  });

  const [projectsOpen, setProjectsOpen] = useState(true);
  const [pinnedOpen, setPinnedOpen] = useState(true);
  const [historyOpen, setHistoryOpen] = useState(true);

  return {
    projects,
    setProjects,
    sessions,
    setSessions,
    sessionsRef,
    sessionTitleOverridesRef: titles.sessionTitleOverridesRef,
    expandedProjects,
    setExpandedProjects,
    visibleSessionsByProject,
    setVisibleSessionsByProject,
    sessionOrder,
    setSessionOrder,
    projectDropHint: drag.projectDropHint,
    setProjectDropHint: drag.setProjectDropHint,
    projectsOpen,
    setProjectsOpen,
    pinnedOpen,
    setPinnedOpen,
    historyOpen,
    setHistoryOpen,
    ctxMenu: menus.ctxMenu,
    setCtxMenu: menus.setCtxMenu,
    showSearch: search.showSearch,
    setShowSearch: search.setShowSearch,
    searchQuery: search.searchQuery,
    setSearchQuery: search.setSearchQuery,
    searchHits: search.searchHits,
    searchTriggerRef: search.searchTriggerRef,
    searchReturnFocusRef: search.searchReturnFocusRef,
    refreshLists,
    refreshSessions,
    refreshProjects,
    sessionsForProject,
    pinnedSessions,
    orphanSessions,
    startSidebarDrag: drag.startSidebarDrag,
    endSidebarDrag: drag.endSidebarDrag,
    dragOverProject: drag.dragOverProject,
    dropProject: drag.dropProject,
    dropSession: drag.dropSession,
    applyProjectOrder: drag.applyProjectOrder,
    openSearch: search.openSearch,
    openSessionMenu: menus.openSessionMenu,
    openProjectMenu: menus.openProjectMenu,
    renameProject: actions.renameProject,
    renameSession: actions.renameSession,
    relocateProject: actions.relocateProject,
    removeProjectFromApp: actions.removeProjectFromApp,
    archiveSession: actions.archiveSession,
    pinSession: actions.pinSession,
    copySessionId: actions.copySessionId,
    viewTrajectory: actions.viewTrajectory,
    applyMessagePrefixTitle: titles.applyMessagePrefixTitle,
    applyAutomaticSessionTitle: titles.applyAutomaticSessionTitle,
    openSession,
    newChat,
  };
}

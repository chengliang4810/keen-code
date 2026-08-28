import { useCallback, type MutableRefObject } from "react";
import type {
  AppDialog,
  ContextMenuState,
  Project,
  SessionRow,
} from "@/features/app/models";
import type { ResourceOpenTarget } from "@/components/ResourceViewer";
import type { Locale } from "@/i18n";
import * as api from "@/lib/api";
import { localizeUiError } from "@/lib/session";
import { updateSessionPreference } from "@/lib/sessionPreferences";
import {
  sessionRename as acpSessionRename,
} from "@/lib/acp/api";
import { saveLayout, type LayoutPrefs } from "@/lib/layout";
import type {
  SidebarNewChat,
  SidebarSetState,
  SidebarShowToast,
  SidebarTranslator,
} from "./types";

export interface SidebarActionsOptions {
  locale: Locale;
  tr: SidebarTranslator;
  activeProject: Project | null;
  projects: Project[];
  currentSessionId: string | null;
  viewingSessionIdRef: MutableRefObject<string | null>;
  setActiveProject: SidebarSetState<Project | null>;
  setAppDialog: SidebarSetState<AppDialog>;
  setLocalError: SidebarSetState<string | null>;
  setLayout: SidebarSetState<LayoutPrefs>;
  setResourceOpenTarget: SidebarSetState<ResourceOpenTarget | null>;
  setVisibleSessionsByProject: SidebarSetState<Record<string, number>>;
  setExpandedProjects: SidebarSetState<Record<string, boolean>>;
  setCtxMenu: SidebarSetState<ContextMenuState>;
  onActiveProjectRelocated?: (project: Project) => void;
  onActiveProjectRemoved?: (project: Project) => void;
  newChat: SidebarNewChat;
  showToast: SidebarShowToast;
  refreshProjects: () => Promise<void>;
  refreshSessions: () => Promise<void>;
  applySessionTitle: (sessionId: string, title: string) => void;
}

export interface SidebarActionsResult {
  renameProject: (project: Project) => void;
  renameSession: (session: SessionRow) => void;
  relocateProject: (project: Project) => Promise<void>;
  removeProjectFromApp: (project: Project) => void;
  archiveSession: (session: SessionRow, archived?: boolean) => Promise<void>;
  pinSession: (session: SessionRow, pinned?: boolean) => Promise<void>;
  copySessionId: (session: SessionRow) => Promise<void>;
  viewTrajectory: (session: SessionRow) => void;
}

export function useSidebarActions({
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
  setCtxMenu,
  onActiveProjectRelocated,
  onActiveProjectRemoved,
  newChat,
  showToast,
  refreshProjects,
  refreshSessions,
  applySessionTitle,
}: SidebarActionsOptions): SidebarActionsResult {
  const renameProject = useCallback(
    (project: Project) => {
      setCtxMenu(null);
      setAppDialog({
        kind: "prompt",
        title: tr("project.rename"),
        initial: project.name,
        onSubmit: async (name) => {
          const next = name.trim();
          if (!next || next === project.name) return;
          try {
            await api.projectRename(project.id, next);
            await refreshProjects();
            if (activeProject?.id === project.id) {
              setActiveProject((previous) =>
                previous ? { ...previous, name: next } : previous,
              );
            }
          } catch (error) {
            setLocalError(localizeUiError(error, locale));
          }
        },
      });
    },
    [
      activeProject?.id,
      locale,
      refreshProjects,
      setActiveProject,
      setAppDialog,
      setCtxMenu,
      setLocalError,
      tr,
    ],
  );

  const renameSession = useCallback(
    (target: SessionRow) => {
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
    },
    [
      applySessionTitle,
      locale,
      refreshSessions,
      setAppDialog,
      setCtxMenu,
      setLocalError,
      tr,
    ],
  );

  const relocateProject = useCallback(
    async (project: Project) => {
      setCtxMenu(null);
      if (!api.isTauri()) {
        setLocalError(tr("error.needTauri"));
        return;
      }
      try {
        const directory = await api.pickDirectory();
        if (!directory) return;
        const updated = await api.projectRelocate(project.id, directory);
        await refreshProjects();
        if (activeProject?.id === project.id) {
          setActiveProject(updated);
          onActiveProjectRelocated?.(updated);
        }
        setLocalError(null);
        showToast(
          tr("project.relocateOk", {
            name: updated.name,
            path: updated.path,
          }),
          3200,
        );
      } catch (error) {
        setLocalError(localizeUiError(error, locale));
      }
    },
    [
      activeProject?.id,
      locale,
      onActiveProjectRelocated,
      refreshProjects,
      setActiveProject,
      setCtxMenu,
      setLocalError,
      showToast,
      tr,
    ],
  );

  const removeProjectFromApp = useCallback(
    (project: Project) => {
      setCtxMenu(null);
      setAppDialog({
        kind: "confirm",
        title: tr("project.removeTitle"),
        message: tr("project.removeConfirmDetail", { name: project.name }),
        confirmLabel: tr("project.remove"),
        danger: true,
        onConfirm: async () => {
          try {
            if (!api.isTauri()) {
              setLocalError(tr("error.needTauri"));
              return;
            }
            await api.projectRemove(project.id);
            setVisibleSessionsByProject((counts) =>
              Object.fromEntries(
                Object.entries(counts).filter(([id]) => id !== project.id),
              ),
            );
            if (activeProject?.id === project.id) {
              setActiveProject(null);
              onActiveProjectRemoved?.(project);
            }
            await refreshProjects();
            await refreshSessions();
            setLocalError(null);
          } catch (error) {
            setLocalError(localizeUiError(error, locale));
          }
        },
      });
    },
    [
      activeProject?.id,
      locale,
      onActiveProjectRemoved,
      refreshProjects,
      refreshSessions,
      setActiveProject,
      setAppDialog,
      setCtxMenu,
      setLocalError,
      setVisibleSessionsByProject,
      tr,
    ],
  );

  const archiveSession = useCallback(
    async (target: SessionRow, archived = true) => {
      setCtxMenu(null);
      const wasViewing =
        archived &&
        (currentSessionId === target.id ||
          viewingSessionIdRef.current === target.id);
      try {
        updateSessionPreference(target.id, { archived });
        await refreshSessions();
        if (wasViewing) {
          const project = target.projectId
            ? projects.find((item) => item.id === target.projectId) ?? null
            : null;
          await newChat(project);
        } else if (!archived && target.projectId) {
          setExpandedProjects((expanded) => ({
            ...expanded,
            [target.projectId!]: true,
          }));
        }
      } catch (error) {
        setLocalError(localizeUiError(error, locale));
      }
    },
    [
      currentSessionId,
      locale,
      newChat,
      projects,
      refreshSessions,
      setCtxMenu,
      setExpandedProjects,
      setLocalError,
      viewingSessionIdRef,
    ],
  );

  const pinSession = useCallback(
    async (target: SessionRow, pinned = true) => {
      setCtxMenu(null);
      try {
        updateSessionPreference(target.id, { pinned });
        await refreshSessions();
      } catch (error) {
        setLocalError(localizeUiError(error, locale));
      }
    },
    [locale, refreshSessions, setCtxMenu, setLocalError],
  );

  const copySessionId = useCallback(
    async (target: SessionRow) => {
      setCtxMenu(null);
      try {
        await navigator.clipboard.writeText(target.id);
      } catch {
        setLocalError(target.id);
      }
    },
    [setCtxMenu, setLocalError],
  );

  const viewTrajectory = useCallback(
    (target: SessionRow) => {
      setCtxMenu(null);
      setLayout((current) => {
        const next = { ...current, asideCollapsed: false };
        saveLayout(localStorage, next);
        return next;
      });
      setResourceOpenTarget({
        type: "trajectory",
        sessionId: target.id,
        title: target.title,
      });
    },
    [setCtxMenu, setLayout, setResourceOpenTarget],
  );

  return {
    renameProject,
    renameSession,
    relocateProject,
    removeProjectFromApp,
    archiveSession,
    pinSession,
    copySessionId,
    viewTrajectory,
  };
}

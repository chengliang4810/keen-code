import type { ArchiveOperation } from "@/modules/ai/lib/archivedConversations";
import type { SessionMeta } from "@/modules/ai/lib/sessions";
import {
  isSessionNavigationLocked,
  manageArchivedSessions,
  useChatStore,
} from "@/modules/ai/store/chatStore";
import { useSpaces, type SpaceMeta } from "@/modules/spaces";

export type ArchiveSnapshot = {
  sessions: SessionMeta[];
  projects: SpaceMeta[];
  locked: boolean;
};

/** 设置覆盖层与主对话共用状态，管理结果仍在成功落盘后才返回。 */
export async function requestArchives(
  operation: ArchiveOperation | "list" = "list",
  ids: string[] = [],
): Promise<ArchiveSnapshot> {
  if (
    !useChatStore.getState().sessionsHydrated ||
    !useSpaces.getState().hydrated
  )
    throw new Error("Conversations are still loading. Try again.");
  if (operation !== "list") await manageArchivedSessions(operation, ids);
  return {
    sessions: useChatStore
      .getState()
      .sessions.filter((session) => session.archived),
    projects: useSpaces.getState().spaces,
    locked:
      isSessionNavigationLocked() || useChatStore.getState().sessionLoading,
  };
}

export async function watchArchives(refresh: () => void): Promise<() => void> {
  const stopSessions = useChatStore.subscribe((next, prev) => {
    if (
      next.sessions !== prev.sessions ||
      next.sessionsHydrated !== prev.sessionsHydrated ||
      next.agentMeta.status !== prev.agentMeta.status ||
      next.sessionSubmitting !== prev.sessionSubmitting ||
      next.sessionLoading !== prev.sessionLoading
    )
      refresh();
  });
  const stopProjects = useSpaces.subscribe((next, prev) => {
    if (next.spaces !== prev.spaces || next.hydrated !== prev.hydrated)
      refresh();
  });
  return () => {
    stopSessions();
    stopProjects();
  };
}

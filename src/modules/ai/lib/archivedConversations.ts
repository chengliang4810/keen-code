import type { SessionMeta } from "@/modules/ai/lib/sessions";
import type { SpaceMeta } from "@/modules/spaces";

export type ArchiveOperation = "restore" | "delete";
export type ArchiveKind = "all" | "project" | "independent";
export type ArchivedGroup = {
  id: string;
  project?: SpaceMeta;
  sessions: SessionMeta[];
};

/** 归档页保留被移除项目的名称和归属，不将其历史对话并入其他项目。 */
export function groupArchivedConversations(
  sessions: readonly SessionMeta[],
  projects: readonly SpaceMeta[],
  search = "",
  kind: ArchiveKind = "all",
  projectFilter = "all",
): ArchivedGroup[] {
  const byProject = new Map(projects.map((project) => [project.id, project]));
  const groups = new Map<string, ArchivedGroup>();
  const query = search.trim().toLocaleLowerCase();
  for (const session of sessions) {
    if (!session.archived) continue;
    const projectId = session.projectless ? undefined : session.projectId;
    if (kind === "project" && !projectId) continue;
    if (kind === "independent" && projectId) continue;
    const id = projectId ? `project:${projectId}` : "independent";
    if (projectFilter !== "all" && projectFilter !== id) continue;
    if (query && !session.title.toLocaleLowerCase().includes(query)) continue;
    const group = groups.get(id) ?? {
      id,
      project: projectId ? byProject.get(projectId) : undefined,
      sessions: [],
    };
    group.sessions.push(session);
    groups.set(id, group);
  }
  return [...groups.values()]
    .sort((a, b) =>
      a.id === "independent"
        ? 1
        : b.id === "independent"
          ? -1
          : (a.project?.name ?? a.id).localeCompare(b.project?.name ?? b.id),
    )
    .map((group) => ({
      ...group,
      sessions: group.sessions.sort(
        (a, b) => b.updatedAt - a.updatedAt || a.id.localeCompare(b.id),
      ),
    }));
}

/** 只管理确认时选中的归档 ID；已消失的记录保持幂等，已恢复的记录拒绝删除。 */
export function changeArchivedConversations(
  sessions: readonly SessionMeta[],
  ids: readonly string[],
  operation: ArchiveOperation,
): SessionMeta[] {
  const selected = new Set(ids);
  if (sessions.some((session) => selected.has(session.id) && !session.archived))
    throw new Error("Archived conversations changed. Refresh and try again.");
  return operation === "delete"
    ? sessions.filter((session) => !selected.has(session.id))
    : sessions.map((session) =>
        selected.has(session.id) ? { ...session, archived: false } : session,
      );
}

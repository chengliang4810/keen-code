import type { SessionMeta } from "./sessions";
import type { SpaceMeta } from "@/modules/spaces/lib/store";
import { workspaceScopeKey } from "@/modules/workspace";

/** 旧会话优先按目录和环境匹配项目，避免重名路径跨 WSL 环境混入同一项目。 */
export function resolveSessionProject(
  session: SessionMeta,
  projects: readonly SpaceMeta[],
  activeProjectId: string | null,
): string | null {
  if (session.projectless) return null;
  if (projects.some((p) => p.id === session.projectId))
    return session.projectId!;
  const match = projects.find(
    (p) =>
      !p.removed &&
      p.root === session.workspaceRoot &&
      (!session.workspaceScope ||
        workspaceScopeKey(p.env) === session.workspaceScope),
  );
  return (
    match?.id ??
    projects.find((p) => p.id === activeProjectId && !p.removed)?.id ??
    projects.find((p) => !p.removed)?.id ??
    null
  );
}

export function projectTaskGroups(
  projects: readonly SpaceMeta[],
  sessions: readonly SessionMeta[],
  archived: boolean,
) {
  return projects
    .filter((project) => !project.removed)
    .map((project) => ({
      project,
      tasks: sessions
        .filter(
          (s) => s.projectId === project.id && Boolean(s.archived) === archived,
        )
        .sort((a, b) => b.updatedAt - a.updatedAt),
    }));
}

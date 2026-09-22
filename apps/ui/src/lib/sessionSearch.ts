/** Pure helpers for the sidebar / command-palette search. */

export type SearchableSession = {
  id: string;
  title: string;
  projectId?: string | null;
  archived?: boolean;
};

export type SearchableProject = {
  id: string;
  name: string;
  path: string;
};

export type SessionSearchHits = {
  matchedSessions: SearchableSession[];
  matchedProjects: SearchableProject[];
};

/**
 * Filter sessions and projects by a free-text query.
 * Matches session title / id, and project name / path.
 * When a query matches a project, its sessions are also included.
 */
export function filterSessionSearch(
  query: string,
  sessions: SearchableSession[],
  projects: SearchableProject[],
  opts?: { maxSessions?: number; maxProjects?: number; includeArchived?: boolean },
): SessionSearchHits {
  const maxSessions = opts?.maxSessions ?? 20;
  const maxProjects = opts?.maxProjects ?? 10;
  const includeArchived = opts?.includeArchived ?? false;

  const live = includeArchived
    ? sessions
    : sessions.filter((s) => !s.archived);

  const q = query.trim().toLowerCase();
  if (!q) {
    return {
      matchedSessions: live.slice(0, Math.min(12, maxSessions)),
      matchedProjects: projects.slice(0, Math.min(6, maxProjects)),
    };
  }

  const projectById = new Map(projects.map((p) => [p.id, p]));
  const matchedProjects = projects
    .filter(
      (p) =>
        p.name.toLowerCase().includes(q) || p.path.toLowerCase().includes(q),
    )
    .slice(0, maxProjects);
  const matchedProjectIds = new Set(matchedProjects.map((p) => p.id));

  const matchedSessions = live
    .filter((s) => {
      if (s.title.toLowerCase().includes(q) || s.id.toLowerCase().includes(q)) {
        return true;
      }
      if (s.projectId && matchedProjectIds.has(s.projectId)) {
        return true;
      }
      // Also match project name even if project list itself is full.
      if (s.projectId) {
        const p = projectById.get(s.projectId);
        if (
          p &&
          (p.name.toLowerCase().includes(q) || p.path.toLowerCase().includes(q))
        ) {
          return true;
        }
      }
      return false;
    })
    .slice(0, maxSessions);

  return { matchedSessions, matchedProjects };
}

/**
 * Sidebar project-folder expand/collapse persistence helpers.
 *
 * Product rule: missing id ⇒ expanded. Only collapsed folders are stored
 * (`sidebarCollapsedProjectIds`) so new projects open by default.
 */

/** Build expand map for known project ids from persisted collapsed ids. */
export function expandMapFromCollapsedIds(
  projectIds: string[],
  collapsedIds: string[] | null | undefined,
): Record<string, boolean> {
  const collapsed = new Set(
    (collapsedIds ?? []).map((id) => id.trim()).filter(Boolean),
  );
  const map: Record<string, boolean> = {};
  for (const id of projectIds) {
    map[id] = !collapsed.has(id);
  }
  return map;
}

/** Ids that should be written to settings (explicitly collapsed). */
export function collapsedIdsFromExpandMap(
  map: Record<string, boolean>,
): string[] {
  return Object.entries(map)
    .filter(([, open]) => open === false)
    .map(([id]) => id)
    .sort();
}

/** True when two collapsed-id lists encode the same set. */
export function sameCollapsedIdSet(a: string[], b: string[]): boolean {
  if (a.length !== b.length) return false;
  const sa = [...a].sort();
  const sb = [...b].sort();
  return sa.every((id, i) => id === sb[i]);
}

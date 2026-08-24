const SESSION_ORDER_KEY = "keencode.sidebar-session-order";

export function loadSessionOrder(storage: Storage = localStorage): string[] {
  const value = storage.getItem(SESSION_ORDER_KEY);
  if (value === null) return [];
  const parsed: unknown = JSON.parse(value);
  if (!Array.isArray(parsed) || parsed.some((id) => typeof id !== "string")) {
    throw new Error("侧栏会话顺序必须是字符串数组");
  }
  return [...new Set(parsed)];
}

export function saveSessionOrder(ids: readonly string[], storage: Storage = localStorage): void {
  storage.setItem(SESSION_ORDER_KEY, JSON.stringify([...new Set(ids)]));
}

export function orderedByIds<T extends { id: string }>(items: readonly T[], ids: readonly string[]): T[] {
  const positions = new Map(ids.map((id, index) => [id, index]));
  return items
    .map((item, index) => ({ item, index, position: positions.get(item.id) }))
    .sort((a, b) => {
      if (a.position === undefined) return b.position === undefined ? a.index - b.index : -1;
      if (b.position === undefined) return 1;
      return a.position - b.position;
    })
    .map(({ item }) => item);
}

export function moveId(ids: readonly string[], source: string, target: string, after = false): string[] {
  if (source === target) return [...ids];
  const next = ids.filter((id) => id !== source);
  const targetIndex = next.indexOf(target);
  if (targetIndex < 0) return [...ids];
  next.splice(targetIndex + (after ? 1 : 0), 0, source);
  return next;
}

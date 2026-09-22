const SESSION_ORDER_KEY = "keencode.sidebar-session-order";
const SESSION_SORT_MODE_KEY = "keencode.sidebar-session-sort-mode";

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

/** 侧栏会话排序方式：用户消息时间优先，或按会话最后更新时间。 */
export type SidebarSortMode = "lastUserMessage" | "updatedAt";

/** 默认按最后用户消息排序：只有用户真的发消息，会话才会浮到最前。 */
export const DEFAULT_SESSION_SORT_MODE: SidebarSortMode = "lastUserMessage";

export function loadSessionSortMode(storage: Storage = localStorage): SidebarSortMode {
  const value = storage.getItem(SESSION_SORT_MODE_KEY);
  if (value === null) return DEFAULT_SESSION_SORT_MODE;
  const parsed: unknown = JSON.parse(value);
  if (parsed !== "lastUserMessage" && parsed !== "updatedAt") {
    throw new Error("侧栏排序方式必须是 lastUserMessage 或 updatedAt");
  }
  return parsed;
}

export function saveSessionSortMode(
  mode: SidebarSortMode,
  storage: Storage = localStorage,
): void {
  storage.setItem(SESSION_SORT_MODE_KEY, JSON.stringify(mode));
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

/** 会话排序使用的时间；缺字段或非法时间视为最旧，不参与抢占前位。 */
function sortTime(row: { updatedAt: string; lastUserMessageAt: string | null }, mode: SidebarSortMode): number {
  // 从未发送消息的会话没有用户消息时间，回退到更新时间避免沉底。
  const source =
    mode === "lastUserMessage" ? row.lastUserMessageAt ?? row.updatedAt : row.updatedAt;
  const value = Date.parse(source);
  return Number.isFinite(value) ? value : 0;
}

/**
 * 拖拽过的会话固定在列表前面并保持拖出的相对顺序；其余会话按所选时间排序。
 * 同值时按稳定标识排序，避免刷新后顺序抖动。
 */
export function sortSessionRows<T extends { id: string; updatedAt: string; lastUserMessageAt: string | null }>(
  items: readonly T[],
  manualOrder: readonly string[],
  mode: SidebarSortMode,
): T[] {
  const positions = new Map(manualOrder.map((id, index) => [id, index]));
  return [...items].sort((left, right) => {
    const leftPosition = positions.get(left.id);
    const rightPosition = positions.get(right.id);
    if (leftPosition !== undefined || rightPosition !== undefined) {
      if (leftPosition === undefined) return 1;
      if (rightPosition === undefined) return -1;
      return leftPosition - rightPosition;
    }
    const diff = sortTime(right, mode) - sortTime(left, mode);
    return diff !== 0 ? diff : left.id.localeCompare(right.id);
  });
}

export function moveId(ids: readonly string[], source: string, target: string, after = false): string[] {
  if (source === target) return [...ids];
  const next = ids.filter((id) => id !== source);
  const targetIndex = next.indexOf(target);
  if (targetIndex < 0) return [...ids];
  next.splice(targetIndex + (after ? 1 : 0), 0, source);
  return next;
}

/**
 * 会话时间线的 renderer-local 滚动位置记忆。
 *
 * 记忆只属于当前前端实例，不写入会话数据或磁盘；使用 LRU 上限避免用户
 * 长时间切换会话后无界保留 DOM 度量。`wasPinnedToBottom` 为 false 时，
 * 恢复会把用户带回上次阅读位置；为 true 时继续使用当前吸底语义。
 */

export interface SessionScrollMemoryState {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
  wasPinnedToBottom: boolean;
  updatedAt: number;
}

/** 单个前端实例最多保留的最近会话数量。 */
export const SESSION_SCROLL_MEMORY_MAX_ENTRIES = 200;

const sessionScrollMemory = new Map<string, SessionScrollMemoryState>();

function normalizeKey(key: string | number | null | undefined): string | null {
  const value = String(key ?? "").trim();
  return value || null;
}

function finiteNonNegative(value: number): number {
  return Number.isFinite(value) && value >= 0 ? value : 0;
}

function normalizeState(state: SessionScrollMemoryState): SessionScrollMemoryState {
  return {
    scrollTop: finiteNonNegative(state.scrollTop),
    scrollHeight: finiteNonNegative(state.scrollHeight),
    clientHeight: finiteNonNegative(state.clientHeight),
    wasPinnedToBottom: state.wasPinnedToBottom === true,
    updatedAt: Number.isFinite(state.updatedAt) ? state.updatedAt : Date.now(),
  };
}

/** 将访问过的条目移到尾部，使 Map 顺序表达最近最少使用顺序。 */
function touch(key: string, state: SessionScrollMemoryState): SessionScrollMemoryState {
  sessionScrollMemory.delete(key);
  sessionScrollMemory.set(key, state);
  return state;
}

function prune(): void {
  while (sessionScrollMemory.size > SESSION_SCROLL_MEMORY_MAX_ENTRIES) {
    const oldestKey = sessionScrollMemory.keys().next().value;
    if (oldestKey === undefined) return;
    sessionScrollMemory.delete(oldestKey);
  }
}

/** 读取并触摸一个会话的滚动记忆。 */
export function readSessionScrollMemory(
  key: string | number | null | undefined,
): SessionScrollMemoryState | null {
  const normalizedKey = normalizeKey(key);
  if (!normalizedKey) return null;
  const state = sessionScrollMemory.get(normalizedKey);
  return state ? touch(normalizedKey, state) : null;
}

/** 保存一个会话的滚动记忆，并淘汰最久未访问的条目。 */
export function saveSessionScrollMemory(
  key: string | number | null | undefined,
  state: SessionScrollMemoryState,
): void {
  const normalizedKey = normalizeKey(key);
  if (!normalizedKey) return;
  touch(normalizedKey, normalizeState(state));
  prune();
}

/** 将保存的位置钳制到当前内容可表示的范围。 */
export function resolveSessionScrollTop(
  state: Pick<SessionScrollMemoryState, "scrollTop">,
  metrics: Pick<HTMLElement, "scrollHeight" | "clientHeight">,
): number {
  const maxScrollTop = Math.max(0, metrics.scrollHeight - metrics.clientHeight);
  return Math.min(Math.max(finiteNonNegative(state.scrollTop), 0), maxScrollTop);
}

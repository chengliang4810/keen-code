/** 未查看终态结果的当前本地存储键。 */
export const UNREAD_TERMINAL_RESULTS_KEY =
  "keencode.unread-terminal-session-results";

/** 后台形成终态但尚未由用户打开查看的回合结果。 */
export type UnreadTerminalResult = "completed" | "failed";

/** 判断 ACP 回合是否为可清理计划、可标记完成的正常结束。 */
export function isNormalSessionCompletion(
  stopReason: string,
  hasError: boolean,
): boolean {
  return stopReason === "end_turn" && !hasError;
}

/** 读取尚未由用户打开查看的终态 Session 结果。 */
export function loadUnreadTerminalResults(
  storage: Storage | null,
): Map<string, UnreadTerminalResult> {
  if (!storage) return new Map();
  const raw = storage.getItem(UNREAD_TERMINAL_RESULTS_KEY);
  if (raw === null) return new Map();
  const value = JSON.parse(raw) as unknown;
  if (!Array.isArray(value)) throw new Error("未查看终态结果无效");
  const results = new Map<string, UnreadTerminalResult>();
  for (const entry of value) {
    if (
      !Array.isArray(entry) ||
      entry.length !== 2 ||
      typeof entry[0] !== "string" ||
      !entry[0] ||
      (entry[1] !== "completed" && entry[1] !== "failed")
    ) {
      throw new Error("未查看终态结果无效");
    }
    results.set(entry[0], entry[1]);
  }
  return results;
}

/** 保存尚未由用户打开查看的终态 Session 结果。 */
export function saveUnreadTerminalResults(
  results: Map<string, UnreadTerminalResult>,
  storage: Storage | null,
): void {
  if (!storage) return;
  storage.setItem(UNREAD_TERMINAL_RESULTS_KEY, JSON.stringify([...results]));
}

/** 损坏数据降级包装：删除坏 key、返回空集合，绝不抛出。 */
export function loadUnreadTerminalResultsSafe(storage: Storage | null): Map<string, UnreadTerminalResult> {
  try {
    return loadUnreadTerminalResults(storage);
  } catch {
    try {
      storage?.removeItem(UNREAD_TERMINAL_RESULTS_KEY);
    } catch {
      // 忽略清理失败；空集合已兜底。
    }
    return new Map();
  }
}

/** 未查看完成状态的当前本地存储键。 */
export const COMPLETED_UNREAD_SESSION_IDS_KEY =
  "keencode.completed-unread-session-ids";

/** 判断 ACP 回合是否为可清理计划、可标记完成的正常结束。 */
export function isNormalSessionCompletion(
  stopReason: string,
  hasError: boolean,
): boolean {
  return stopReason === "end_turn" && !hasError;
}

/** 读取尚未由用户打开查看的正常完成 Session。 */
export function loadCompletedUnreadSessionIds(
  storage: Storage | null,
): Set<string> {
  if (!storage) return new Set();
  const raw = storage.getItem(COMPLETED_UNREAD_SESSION_IDS_KEY);
  if (raw === null) return new Set();
  const value = JSON.parse(raw) as unknown;
  if (
    !Array.isArray(value) ||
    value.some((sessionId) => typeof sessionId !== "string" || !sessionId)
  ) {
    throw new Error("未查看完成任务状态无效");
  }
  return new Set(value);
}

/** 保存尚未由用户打开查看的正常完成 Session。 */
export function saveCompletedUnreadSessionIds(
  sessionIds: Set<string>,
  storage: Storage | null,
): void {
  if (!storage) return;
  storage.setItem(
    COMPLETED_UNREAD_SESSION_IDS_KEY,
    JSON.stringify([...sessionIds]),
  );
}

/** Goal 列表与 mutation 共用的会话级同步纪元。 */

/**
 * Goal 创建发生在 Composer hook 之外（例如首条消息发送或队列 steering），
 * 因此用模块级、按 Session 隔离的纪元通知在途 goals.list 失效。
 */
const goalMutationEpochs = new Map<string, number>();

/** 生成 Goal mutation 共用的幂等请求标识。 */
export function createGoalRequestNonce(): string {
  return `keencode-goal-${Date.now()}-${Math.random()
    .toString(36)
    .slice(2)}`;
}

/** 返回指定 Session 当前的 Goal mutation 纪元。 */
export function getGoalMutationEpoch(sessionId: string): number {
  return goalMutationEpochs.get(sessionId) ?? 0;
}

/** 推进指定 Session 的 Goal mutation 纪元，使已发出的列表响应过期。 */
export function invalidateGoalListRequests(sessionId: string): number {
  const next = getGoalMutationEpoch(sessionId) + 1;
  goalMutationEpochs.set(sessionId, next);
  return next;
}

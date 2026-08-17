/**
 * 用 Host 快照校准前端的易失 turn 关联。
 *
 * 本地刚开始的 send 可能晚于快照请求，必须优先保留；除此之外 Host 是
 * 最终真相，可以清掉漏收 done 后的旧 ID，也可以覆盖迟到事件留下的旧 ID。
 */
export function resolveActiveTurnFromHostSnapshot(args: {
  snapshotTurnId: string | null | undefined;
  localTurnId: string | null | undefined;
  completedTurnId: string | null | undefined;
}): string | null {
  const { snapshotTurnId, localTurnId, completedTurnId } = args;
  const activeLocalTurnId =
    localTurnId && localTurnId !== completedTurnId ? localTurnId : null;
  if (activeLocalTurnId && activeLocalTurnId !== snapshotTurnId) {
    return activeLocalTurnId;
  }
  if (!snapshotTurnId || snapshotTurnId === completedTurnId) {
    return activeLocalTurnId;
  }
  return snapshotTurnId;
}

/**
 * 监听器已注册但 Host 快照尚未返回时，暂存无法校验的 turn-scoped 事件。
 * 快照落地后只按原始到达顺序重放仍与 Host active turn 精确匹配的事件。
 */
export function createActiveTurnBootstrapBuffer(
  getActiveTurnId: (sessionId: string) => string | null | undefined,
  maxEvents = 4096,
) {
  const events: Array<{
    sessionId: string;
    turnId: string;
    apply: () => void;
  }> = [];
  let pending = true;
  let overflowed = false;

  return {
    deferUnknown(
      sessionId: string,
      turnId: string | null | undefined,
      apply: () => void,
    ): boolean {
      if (getActiveTurnId(sessionId)) return false;
      if (!pending || !turnId) return true;
      if (events.length >= maxEvents) {
        overflowed = true;
        return true;
      }
      events.push({ sessionId, turnId, apply });
      return true;
    },

    replayMatching(): void {
      pending = false;
      const queued = events.splice(0);
      for (const event of queued) {
        if (getActiveTurnId(event.sessionId) === event.turnId) {
          event.apply();
        }
      }
    },

    discard(): void {
      pending = false;
      events.length = 0;
    },

    get overflowed(): boolean {
      return overflowed;
    },
  };
}

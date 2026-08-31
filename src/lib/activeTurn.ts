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

/** Host 快照中与运行回合恢复有关的最小字段。 */
export interface HostActiveTurnSnapshot {
  /** 快照所属 Session；缺失时不得修改任何状态。 */
  sessionId?: string | null;
  /** Host 当前活跃回合；null 表示 Host 当前没有运行回合。 */
  activeTurnId?: string | null;
}

/** 运行回合恢复需要读取或更新的四组 Session Map。 */
export interface ActiveTurnSnapshotMaps {
  /** 本地刚发送、可能晚于 Host 快照开始的回合观测。 */
  turnLatencyBySession: ReadonlyMap<string, { readonly turnId: string }>;
  /** 当前用于路由实时事件的活跃回合。 */
  activeTurnIdBySession: Map<string, string>;
  /** Host 已完成但尾随事件仍可恢复的回合。 */
  recoverableCompletedTurnIdBySession: Map<string, string>;
  /** 前端已经消费完成事件的回合。 */
  completedTurnIdBySession: ReadonlyMap<string, string>;
}

/**
 * 使用 canonical 解析规则把 Host 快照原子地收敛到全部 Active Turn Map。
 */
export function reconcileHostActiveTurnSnapshot(
  snapshot: HostActiveTurnSnapshot,
  maps: ActiveTurnSnapshotMaps,
): string | null {
  const sessionId = snapshot.sessionId;
  if (!sessionId) return null;
  const resolved = resolveActiveTurnFromHostSnapshot({
    snapshotTurnId: snapshot.activeTurnId,
    localTurnId: maps.turnLatencyBySession.get(sessionId)?.turnId,
    completedTurnId: maps.completedTurnIdBySession.get(sessionId),
  });
  if (resolved) maps.activeTurnIdBySession.set(sessionId, resolved);
  else maps.activeTurnIdBySession.delete(sessionId);
  if (
    resolved &&
    maps.recoverableCompletedTurnIdBySession.get(sessionId) !== resolved
  ) {
    maps.recoverableCompletedTurnIdBySession.delete(sessionId);
  }
  return resolved;
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

import { useCallback, useEffect } from "react";
import type { ChatMessage, SessionSnapshot } from "@/lib/session";
import type { Ref, SetState, SessionMessageReducer } from "./types";

export interface AcpRuntimeMessageCacheOptions {
  session: SessionSnapshot;
  messages: ChatMessage[];
  messagesRef: Ref<ChatMessage[]>;
  messagesBySessionRef: Ref<Map<string, ChatMessage[]>>;
  viewingSessionIdRef: Ref<string | null>;
  setMessages: SetState<ChatMessage[]>;
}

/**
 * 限制非活动 Session 的消息投影数量；Host/Journal 仍是事实源，被淘汰项在
 * 下次打开时走既有 session/load 恢复。运行中、排队或待回答会话由调用方保护。
 */
export function pruneInactiveSessionMessageCache(
  cache: Map<string, ChatMessage[]>,
  protectedSessionIds: ReadonlySet<string>,
  maxInactive = 6,
): string[] {
  const inactive = [...cache.keys()].filter(
    (sessionId) => sessionId !== "__draft__" && !protectedSessionIds.has(sessionId),
  );
  const removeCount = Math.max(0, inactive.length - Math.max(0, maxInactive));
  const removed = inactive.slice(0, removeCount);
  for (const sessionId of removed) cache.delete(sessionId);
  return removed;
}

/** 汇总不可回收的会话后执行缓存裁剪，避免装配层重复维护保护集合。 */
export function pruneUnprotectedSessionMessageCache(
  cache: Map<string, ChatMessage[]>,
  busySessionIds: Iterable<string>,
  queuedSessionIds: Iterable<string>,
  activeSessionId: string | null | undefined,
  pendingAskUserSessionIds: Iterable<string>,
): string[] {
  const protectedSessionIds = new Set([...busySessionIds, ...queuedSessionIds]);
  if (activeSessionId) protectedSessionIds.add(activeSessionId);
  for (const sessionId of pendingAskUserSessionIds) protectedSessionIds.add(sessionId);
  return pruneInactiveSessionMessageCache(cache, protectedSessionIds);
}

/** 维护当前消息引用与按 Session 分区的内存缓存。 */
export function useAcpRuntimeMessageCache({
  session,
  messages,
  messagesRef,
  messagesBySessionRef,
  viewingSessionIdRef,
  setMessages,
}: AcpRuntimeMessageCacheOptions): {
  patchSessionMessages: (
    targetSessionId: string | undefined | null,
    reduce: SessionMessageReducer,
  ) => void;
} {
  useEffect(() => {
    messagesRef.current = messages;
    const sessionId = session.sessionId;
    if (!sessionId) return;
    messagesBySessionRef.current.set(sessionId, messages);
  }, [messages, session.sessionId]);

  const patchSessionMessages = useCallback(
    (
      targetSessionId: string | undefined | null,
      reduce: SessionMessageReducer,
    ) => {
      if (!targetSessionId) return;
      if (viewingSessionIdRef.current === targetSessionId) {
        setMessages((previous) => {
          const next = reduce(previous);
          messagesBySessionRef.current.set(targetSessionId, next);
          return next;
        });
      } else {
        const previous = messagesBySessionRef.current.get(targetSessionId) ?? [];
        messagesBySessionRef.current.set(targetSessionId, reduce(previous));
      }
    },
    [],
  );

  return { patchSessionMessages };
}

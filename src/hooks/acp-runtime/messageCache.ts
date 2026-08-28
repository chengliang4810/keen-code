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

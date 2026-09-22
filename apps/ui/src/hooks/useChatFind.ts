import { useCallback, useEffect, useMemo, useState } from "react";
import type { ChatMessage } from "@/lib/session";
import {
  findChatMatches,
  stepChatFindIndex,
  type ChatFindMatch,
} from "@/lib/chatFind";

export type UseChatFindOptions = {
  messages: ChatMessage[];
  sessionId: string | null;
  dialogOpen?: boolean;
};

export type ChatFindActive = {
  messageId: string;
  occurrence: number;
} | null;

/** 管理当前会话内查找及其消息高亮投影。 */
export function useChatFind({
  messages,
  sessionId,
  dialogOpen = false,
}: UseChatFindOptions) {
  const [showChatFind, setShowChatFind] = useState(false);
  const [chatFindQuery, setChatFindQuery] = useState("");
  const [chatFindIndex, setChatFindIndex] = useState(0);
  const [chatFindFocusKey, setChatFindFocusKey] = useState(0);

  const chatFindMatches = useMemo((): ChatFindMatch[] => {
    if (!showChatFind) return [];
    return findChatMatches(
      chatFindQuery,
      messages
        .filter(
          (message) =>
            message.role === "user" || message.role === "assistant",
        )
        .map((message) => ({
          id: message.id,
          role: message.role,
          content: message.content,
          marker: message.marker,
        })),
    );
  }, [showChatFind, chatFindQuery, messages]);

  const chatFindHitIds = useMemo(() => {
    const hitIds = new Set<string>();
    for (const match of chatFindMatches) hitIds.add(match.messageId);
    return hitIds;
  }, [chatFindMatches]);

  const chatFindActive = useMemo<ChatFindActive>(() => {
    if (!showChatFind || chatFindMatches.length === 0) return null;
    const index =
      chatFindIndex >= 0 && chatFindIndex < chatFindMatches.length
        ? chatFindIndex
        : 0;
    const match = chatFindMatches[index]!;
    return { messageId: match.messageId, occurrence: match.occurrence };
  }, [showChatFind, chatFindMatches, chatFindIndex]);

  // Clamp the active index when query results shrink.
  useEffect(() => {
    if (!showChatFind) return;
    if (chatFindMatches.length === 0) {
      if (chatFindIndex !== 0) setChatFindIndex(0);
      return;
    }
    if (chatFindIndex >= chatFindMatches.length) setChatFindIndex(0);
  }, [showChatFind, chatFindMatches.length, chatFindIndex]);

  // A conversation switch starts a fresh find state.
  useEffect(() => {
    setShowChatFind(false);
    setChatFindQuery("");
    setChatFindIndex(0);
  }, [sessionId]);

  useEffect(() => {
    if (!showChatFind) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.isComposing || dialogOpen) return;
      event.preventDefault();
      event.stopPropagation();
      setShowChatFind(false);
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [dialogOpen, showChatFind]);

  const openChatFind = useCallback(() => {
    setShowChatFind(true);
    setChatFindFocusKey((key) => key + 1);
  }, []);

  const chatFindNext = useCallback(() => {
    setChatFindIndex((index) =>
      stepChatFindIndex(index, chatFindMatches.length, 1),
    );
  }, [chatFindMatches.length]);

  const chatFindPrev = useCallback(() => {
    setChatFindIndex((index) =>
      stepChatFindIndex(index, chatFindMatches.length, -1),
    );
  }, [chatFindMatches.length]);

  return {
    showChatFind,
    setShowChatFind,
    chatFindQuery,
    setChatFindQuery,
    chatFindIndex,
    setChatFindIndex,
    chatFindFocusKey,
    chatFindMatches,
    chatFindHitIds,
    chatFindActive,
    openChatFind,
    chatFindNext,
    chatFindPrev,
  };
}

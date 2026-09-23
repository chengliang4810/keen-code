import { useCallback, useRef } from "react";
import { hasUnreadyAttachments, type Attachment } from "@/lib/attachments";
import { isDraftEmpty, parseStoredContent, serializeForAgent } from "@/lib/draftDoc";
import { buildGoalDraft } from "@/lib/goalDraft";
import { localizeUiError } from "@/lib/session";
import type { Locale } from "@/i18n";
import { shouldEnqueueSend } from "@/lib/sendQueue";
import { isViewingSendTarget } from "@/lib/viewFocus";
import type { SessionSnapshot } from "@/lib/session";
import type {
  ExecuteSend,
  Ref,
  SessionTurnQueuePort,
  SessionTurnRuntimePort,
  SessionTurnUiPort,
} from "./types";

export interface UseSessionDraftSendOptions {
  locale: Locale;
  sessionId: string | null;
  sessionState: SessionSnapshot["state"];
  connecting: boolean;
  draft: string;
  attachments: Attachment[];
  hasConfiguredModel: boolean;
  goalModeSessionKey: string | null;
  planModeSessionKey: string | null;
  ultraModeSessionKey: string | null;
  executeSend: ExecuteSend;
  runtime: Pick<SessionTurnRuntimePort, "currentViewFocus">;
  sendQueue: SessionTurnQueuePort;
  ui: Pick<
    SessionTurnUiPort,
    | "setDraft"
    | "setLocalError"
    | "setAttachments"
    | "setGoalModeSessionKey"
    | "promptHistoryIndexRef"
    | "setPromptHistoryIndex"
    | "setPromptHistoryOpen"
    | "setPromptHistoryFilter"
    | "setPromptHistoryActive"
    | "setPromptHistoryFocusFilter"
  >;
}

export interface SessionDraftSendResult {
  send: () => Promise<void>;
  sendRef: Ref<(() => Promise<void>) | null>;
}

export function useSessionDraftSend({
  locale,
  sessionId,
  sessionState,
  connecting,
  draft,
  attachments,
  hasConfiguredModel,
  goalModeSessionKey,
  planModeSessionKey,
  ultraModeSessionKey,
  executeSend,
  runtime,
  sendQueue,
  ui,
}: UseSessionDraftSendOptions): SessionDraftSendResult {
  const {
    currentViewFocus,
  } = runtime;
  const {
    setDraft,
    setAttachments,
    setGoalModeSessionKey,
    promptHistoryIndexRef,
    setPromptHistoryIndex,
    setPromptHistoryOpen,
    setPromptHistoryFilter,
    setPromptHistoryActive,
    setPromptHistoryFocusFilter,
  } = ui;

  const clearComposerAfterSubmit = useCallback(() => {
    setDraft("");
    setGoalModeSessionKey(null);
    promptHistoryIndexRef.current = null;
    setPromptHistoryIndex(null);
    setPromptHistoryOpen(false);
    setPromptHistoryFilter("");
    setPromptHistoryActive(0);
    setPromptHistoryFocusFilter(false);
    if (typeof requestAnimationFrame === "function") {
      requestAnimationFrame(() => {
        const element = document.querySelector<HTMLElement>(".composer__input");
        if (element) element.style.height = "auto";
      });
    }
    setAttachments([]);
  }, [
    promptHistoryIndexRef,
    setAttachments,
    setDraft,
    setGoalModeSessionKey,
    setPromptHistoryActive,
    setPromptHistoryFilter,
    setPromptHistoryFocusFilter,
    setPromptHistoryIndex,
    setPromptHistoryOpen,
  ]);

  const sendRef = useRef<(() => Promise<void>) | null>(null);
  const send = useCallback(async () => {
    const key = sessionId ?? "__draft__";
    const createGoal = goalModeSessionKey === key;
    const planMode = planModeSessionKey === key;
    const ultraMode = ultraModeSessionKey === key;
    const storedDisplay = draft;
    const segments = parseStoredContent(storedDisplay);
    const att = attachments;
    if (isDraftEmpty(segments) && !att.length) return;
    if (hasUnreadyAttachments(att)) return;
    if (!hasConfiguredModel) return;
    // 本地参数错误保留输入、附件和 Goal 开关，且不得进入队列。
    if (createGoal) {
      try { buildGoalDraft(serializeForAgent(segments)); }
      catch (error) { ui.setLocalError(localizeUiError(error, locale)); return; }
    }
    sendQueue.releaseFlushHold();
    if (shouldEnqueueSend(sessionState, connecting)) {
      sendQueue.enqueue({
        storedDisplay,
        attachments: att,
        createGoal,
        planMode,
        ultraMode,
      });
      clearComposerAfterSubmit();
      return;
    }
    clearComposerAfterSubmit();
    // 直接发送失败或抛异常时输入框与时间线都已清空：把原文与附件回填输入框，
    // 与队列路径保留消息的行为对齐，避免用户文字丢失。
    const originView = currentViewFocus();
    const stillViewing = () =>
      isViewingSendTarget(originView, currentViewFocus(), sessionId);
    try {
      const sent = await executeSend({
        storedDisplay,
        att,
        createGoal,
        planMode,
        ultraMode,
        targetSessionId: sessionId,
      });
      if (sent) return;
    } catch (cause) {
      if (!stillViewing()) return;
      ui.setLocalError(localizeUiError(cause, locale));
    }
    // 等待连接期间用户已切走时不得回填，避免把旧会话的草稿写进新视图。
    if (!stillViewing()) return;
    setDraft(storedDisplay);
    setAttachments(att);
    if (createGoal) setGoalModeSessionKey(key);
  }, [
    attachments,
    clearComposerAfterSubmit,
    connecting,
    currentViewFocus,
    draft,
    executeSend,
    goalModeSessionKey,
    locale,
    ui.setLocalError,
    hasConfiguredModel,
    planModeSessionKey,
    sendQueue,
    sessionId,
    sessionState,
    ultraModeSessionKey,
  ]);
  sendRef.current = send;

  return { send, sendRef };
}

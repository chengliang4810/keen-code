/**
 * LobeHub-aligned chat thread (pure CSS 1:1).
 * Replaces AI Elements / previous ConversationThread.
 */

import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import {
  formatTurnErrorBody,
  localizeSystemNotification,
  isToolInlinedInAssistants,
  messageSegments,
  isTurnPromptMessage,
  type ChatMessage,
  type SessionState,
} from "@/lib/session";
import { isEndOfTurnMarker } from "@/lib/endOfTurn";
import type { Attachment } from "@/lib/attachments";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  buildInlineMediaPathMap,
  filterAttachmentsNotInlined,
  isImagePath,
  isMediaPath,
} from "@/lib/attachments";
import { AttachmentCard } from "@/components/AttachmentCard";
import type { ResourceOpenTarget } from "@/components/ResourceViewer";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import {
  IconArrowsMinimize,
  IconInfo,
  IconRename,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { formatMessageTime } from "@/lib/messageTime";
import { formatTokenCount } from "@/lib/contextUsage";
import { useStickToBottom } from "@/hooks/useStickToBottom";
import { useChatMessageVirtualizer } from "@/hooks/useChatMessageVirtualizer";
import { estimateChatRowHeight } from "@/lib/chatVirtualList";
import {
  MessageActionButton,
  MessageCopyButton,
} from "./MessageAction";
import { ChatItem } from "./ChatItem";
import { MarkdownChat } from "./MarkdownChat";
import { Thinking } from "./Thinking";
import {
  hasDisplayableTurnMetrics,
  TurnMetrics,
} from "./TurnMetrics";
import { BackBottom } from "./BackBottom";
import { SkillChip } from "@/components/SkillChip";
import { HighlightedText } from "@/components/HighlightedText";
import { findChatMatches } from "@/lib/chatFind";
import { hydrateDisplayContent, parseStoredContent } from "@/lib/draftDoc";
import {
  LiveToolText,
} from "./AgentActivity";
import { isToolStepMessage, pickRunningTurnTool } from "@/lib/session";
import { EndOfTurnChip } from "./EndOfTurnChip";
import {
  TimelineToolRow,
  isComposerStateTool,
  latestSubagentToolCallIds,
  subagentForTool,
  toolSegmentFromMessage,
  toolSegmentIsRunning,
} from "./TimelineToolRow";
import { TimelinePhaseBlock } from "./TimelinePhaseBlock";
import { buildTimelineUnits } from "@/lib/timelinePhases";
import { writeUserMessageSelectionToClipboard } from "./userMessageCopy";
import "./lobe-chat.css";

type AttachLabels = {
  open: string;
  reveal: string;
  copyPath: string;
  copyImage: string;
  addToComposer: string;
  remove: string;
};

type ConversationRetryStatus = {
  attempt: number;
  maxAttempts: number;
  reason: string;
};

/**
 * A retry is a transient part of the current turn, so keep it in the chat
 * timeline rather than the window chrome. The ACP event's attempt is the
 * request that just failed; the visible number is the request about to run.
 */
function RetryStatus({
  locale,
  retryStatus,
}: {
  locale: Locale;
  retryStatus?: ConversationRetryStatus | null;
}) {
  const label = retryStatus
    ? (() => {
        const tr = createT(locale);
        const maxAttempts = Number.isFinite(retryStatus.maxAttempts)
          ? Math.max(1, Math.floor(retryStatus.maxAttempts))
          : 10;
        const failedAttempt = Number.isFinite(retryStatus.attempt)
          ? Math.max(0, Math.floor(retryStatus.attempt))
          : 0;
        const nextAttempt = Math.min(failedAttempt + 1, maxAttempts);
        return tr("chat.retryingAttempt", {
          attempt: nextAttempt,
          max: maxAttempts,
        });
      })()
    : "";
  const reason = retryStatus?.reason.trim() ?? "";

  return (
    <div
      className="lobe-chat-retry-status"
      data-testid="chat-retry-status"
      role="status"
      aria-live="polite"
      aria-atomic="true"
      aria-label={label ? (reason ? `${label}: ${reason}` : label) : undefined}
      title={reason || undefined}
    >
      {label ? (
        <span className="lobe-chat-retry-status__label">{label}</span>
      ) : null}
    </div>
  );
}

/** Provider 有时会在正文后发出单独的标点 reasoning delta，完成后不应投影为思考块。 */
function hasMeaningfulThinkingText(text: string): boolean {
  return /[\p{L}\p{N}]/u.test(text);
}

/**
 * Assistant markdown + attachment cards.
 * Memoized so parent re-renders (showBack, live tool pulse, etc.) do not
 * rebuild imagePathMap / remount ImageUi frames mid-scroll.
 */
const AssistantMessageBody = memo(function AssistantMessageBody({
  content,
  attachments,
  streaming,
  locale,
  projectPath,
  onOpenResource,
  onAddAttachmentToComposer,
  attachLabels,
  findQuery,
  findActiveOccurrence,
  findOccurrenceBase = 0,
  onFirstVisibleToken,
  latencyTurnId,
}: {
  content: string;
  attachments?: Attachment[];
  streaming?: boolean;
  locale: Locale;
  projectPath?: string | null;
  onOpenResource?: (target: ResourceOpenTarget) => void;
  onAddAttachmentToComposer?: (att: Attachment) => void;
  attachLabels: AttachLabels;
  findQuery?: string;
  findActiveOccurrence?: number | null;
  /** Offset into the message-level occurrence index for multi-segment bodies. */
  findOccurrenceBase?: number;
  onFirstVisibleToken?: (turnId: string) => void;
  latencyTurnId?: string;
}) {
  const displayContent = content;
  const imagePathMap = useMemo(
    () => buildInlineMediaPathMap(attachments),
    [attachments],
  );
  const bottomAtts = useMemo(
    () =>
      filterAttachmentsNotInlined(displayContent || content, attachments),
    [displayContent, content, attachments],
  );
  const pathMapProp = useMemo(() => {
    return Object.keys(imagePathMap).length ? imagePathMap : undefined;
  }, [imagePathMap]);
  const galleryPaths = useMemo(
    () =>
      (bottomAtts ?? [])
        .filter((x) => !x.isDir && isImagePath(x.path))
        .map((x) => x.path),
    [bottomAtts],
  );

  if (!(displayContent || "").trim() && !(bottomAtts && bottomAtts.length)) {
    return null;
  }

  return (
    <>
      {(displayContent || "").trim() ? (
        <MarkdownChat
          locale={locale}
          streaming={!!streaming}
          imagePathMap={pathMapProp}
          projectPath={projectPath}
          onOpenResource={onOpenResource}
          findQuery={findQuery}
          findActiveOccurrence={findActiveOccurrence}
          findOccurrenceBase={findOccurrenceBase}
          onFirstVisibleToken={onFirstVisibleToken}
          latencyTurnId={latencyTurnId}
        >
          {displayContent}
        </MarkdownChat>
      ) : null}
      {bottomAtts && bottomAtts.length > 0 ? (
        <div className="lobe-chat-atts">
          {bottomAtts.map((a) => (
            <AttachmentCard
              key={a.path}
              attachment={a}
              variant={!a.isDir && isMediaPath(a.path) ? "card" : "chip"}
              labels={attachLabels}
              galleryPaths={galleryPaths}
              onAddToComposer={onAddAttachmentToComposer}
            />
          ))}
        </div>
      ) : null}
    </>
  );
});

/** Render skill chips / plain text for the user bubble body. */
function UserPlainOrSkills({
  content,
  findQuery,
  findActiveOccurrence,
}: {
  content: string;
  findQuery?: string;
  findActiveOccurrence?: number | null;
}) {
  const hydrated = hydrateDisplayContent(content);
  const segs = parseStoredContent(hydrated);
  if (!segs.some((s) => s.type === "skill")) {
    return (
      <span className="user-msg-body">
        {findQuery?.trim() ? (
          <HighlightedText
            text={content}
            query={findQuery}
            activeOccurrence={findActiveOccurrence ?? null}
          />
        ) : (
          content
        )}
      </span>
    );
  }
  return (
    <span className="user-msg-body">
      {segs.map((s, i) =>
        s.type === "skill" ? (
          <SkillChip key={`sk-${i}-${s.name}`} name={s.name} size="sm" />
        ) : findQuery?.trim() && s.text ? (
          <HighlightedText
            key={`t-${i}`}
            text={s.text}
            query={findQuery}
            activeOccurrence={findActiveOccurrence ?? null}
          />
        ) : (
          <span key={`t-${i}`}>{s.text}</span>
        ),
      )}
    </span>
  );
}

function UserMessageEditor({
  initialValue,
  locale,
  onCancel,
  onSend,
}: {
  initialValue: string;
  locale: Locale;
  onCancel: () => void;
  onSend: (value: string) => Promise<boolean>;
}) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [value, setValue] = useState(initialValue);
  const [submitting, setSubmitting] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const canSend = value.trim().length > 0 && !submitting;

  useEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea) return;
    textarea.focus();
    textarea.setSelectionRange(textarea.value.length, textarea.value.length);
  }, []);

  const submit = useCallback(async () => {
    if (!canSend) return;
    setSubmitting(true);
    try {
      if (await onSend(value.trim())) onCancel();
    } finally {
      setSubmitting(false);
    }
  }, [canSend, onCancel, onSend, value]);

  return (
    <div className="lobe-chat-user-editor" data-testid="user-message-editor">
      <Textarea
        ref={textareaRef}
        value={value}
        aria-label={tr("message.editInput")}
        disabled={submitting}
        onChange={(event) => setValue(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onCancel();
          } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
            event.preventDefault();
            void submit();
          }
        }}
      />
      <div className="lobe-chat-user-editor__actions">
        <Button
          type="button"
          className="btn btn--ghost"
          disabled={submitting}
          onClick={onCancel}
        >
          {tr("common.cancel")}
        </Button>
        <Button
          type="button"
          className="btn btn--solid"
          disabled={!canSend}
          onClick={() => void submit()}
        >
          {submitting ? tr("message.sending") : tr("message.send")}
        </Button>
      </div>
    </div>
  );
}

export interface ConversationThreadProps {
  locale: Locale;
  messages: ChatMessage[];
  sessionState: SessionState;
  sessionKey?: string;
  projectPath?: string | null;
  /** When true, suppress generic empty copy (brand mark lives above composer). */
  suppressEmptyCopy?: boolean;
  onOpenResource?: (
    target: import("@/components/ResourceViewer").ResourceOpenTarget,
  ) => void;
  onAddAttachmentToComposer?: (att: Attachment) => void;
  attachLabels: {
    open: string;
    reveal: string;
    copyPath: string;
    copyImage: string;
    addToComposer: string;
    remove: string;
  };
  /**
   * Epoch ms when current agent turn started.
   * Retained for callers; not rendered in the transcript.
   */
  turnStartedAt?: number | null;
  /** 当前模型请求的瞬时重试状态；恢复输出或结束回合后自动清除。 */
  retryStatus?: ConversationRetryStatus | null;
  /** In-chat find (Cmd/Ctrl+F) — highlight + scroll. */
  findQuery?: string;
  /** Message ids that contain at least one match. */
  findHitMessageIds?: ReadonlySet<string>;
  /** Active match target for scroll / current mark. */
  findActive?: { messageId: string; occurrence: number } | null;
  /** Open session Changes panel (turn activity file strip). */
  onOpenSessionChanges?: () => void;
  /** Open a modified path from turn activity. */
  onOpenModifiedPath?: (path: string) => void;
  /** 首段主 Agent reasoning/正文实际提交到 DOM。 */
  onFirstVisibleToken?: (turnId: string) => void;
  /** 当前运行回合的稳定标识，确保迟到 DOM effect 不污染下一轮。 */
  activeTurnId?: string;
  /** 编辑并重新发送当前轨迹的最后一条真实用户消息。 */
  onEditLastUserMessage?: (
    message: ChatMessage,
    content: string,
  ) => Promise<boolean>;
  /** 当前会话中的子智能体，用于替换 Agent 工具调用行。 */
  subagents?: AcpSubagentInfo[];
  /** 本会话实际使用的模型展示名。 */
  modelLabel?: string;
}

/** 将回合耗时锚定到同一用户回合的首条 Assistant 记录。 */
export function processingDurationAnchors(
  messages: ChatMessage[],
): Map<string, number> {
  const anchors = new Map<string, number>();
  let firstAssistantId: string | null = null;
  for (const message of messages) {
    if (message.role === "user") {
      firstAssistantId = null;
      continue;
    }
    if (message.role !== "assistant" || message.isError) continue;
    firstAssistantId ??= message.id;
    if (message.thinkingDurationMs != null) {
      anchors.set(firstAssistantId, message.thinkingDurationMs);
    }
  }
  return anchors;
}

export function ConversationThread({
  locale,
  messages,
  sessionState,
  sessionKey,
  projectPath,
  suppressEmptyCopy = false,
  onOpenResource,
  onAddAttachmentToComposer,
  attachLabels,
  turnStartedAt = null,
  retryStatus = null,
  findQuery = "",
  findHitMessageIds,
  findActive = null,
  onOpenSessionChanges: _onOpenSessionChanges,
  onOpenModifiedPath: _onOpenModifiedPath,
  onFirstVisibleToken,
  activeTurnId,
  onEditLastUserMessage,
  subagents = [],
  modelLabel,
}: ConversationThreadProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const chatRootRef = useRef<HTMLDivElement>(null);
  const [editingUserMessageId, setEditingUserMessageId] = useState<
    string | null
  >(null);
  void _onOpenSessionChanges;
  void _onOpenModifiedPath;

  useEffect(() => {
    const root = chatRootRef.current;
    if (!root) return;
    const ownerDocument = root.ownerDocument;
    const onCopy = (event: globalThis.ClipboardEvent) => {
      if (!event.clipboardData) return;
      const replaced = writeUserMessageSelectionToClipboard(
        root,
        ownerDocument.getSelection(),
        event.clipboardData,
      );
      if (replaced) event.preventDefault();
    };

    // 拖选聊天正文后，焦点仍可能停留在输入框，因此必须监听 document。
    ownerDocument.addEventListener("copy", onCopy, true);
    return () => ownerDocument.removeEventListener("copy", onCopy, true);
  }, []);

  // Scroll the current find match into view (mark if present, else message).
  useEffect(() => {
    if (!findActive?.messageId) return;
    const q = findQuery.trim();
    if (!q) return;
    const id = findActive.messageId;
    const t = window.requestAnimationFrame(() => {
      const root = document.querySelector(
        `[data-message-id="${CSS.escape(id)}"]`,
      ) as HTMLElement | null;
      if (!root) return;
      const currentMark = root.querySelector(
        '[data-find-mark="current"]',
      ) as HTMLElement | null;
      const target = currentMark ?? root;
      target.scrollIntoView({ block: "center", behavior: "smooth" });
    });
    return () => window.cancelAnimationFrame(t);
  }, [findActive?.messageId, findActive?.occurrence, findQuery]);

  // Re-pin when user sends (even if they had scrolled up to read history).
  const forceStickKey = useMemo(() => {
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i]?.role === "user") return messages[i]!.id;
    }
    return null;
  }, [messages]);

  const {
    viewportRef: scrollRef,
    contentRef,
    scrollToBottom,
    isPinnedRef,
    showBack,
  } = useStickToBottom({
    conversationKey: sessionKey ?? "chat",
    forceStickKey,
  });

  const turnBusy = sessionState === "streaming";
  const lastUserMessageId = useMemo(() => {
    for (let index = messages.length - 1; index >= 0; index -= 1) {
      if (messages[index]?.role === "user") return messages[index]!.id;
    }
    return null;
  }, [messages]);

  useEffect(() => {
    if (
      editingUserMessageId &&
      (!messages.some((message) => message.id === editingUserMessageId) ||
        turnBusy)
    ) {
      setEditingUserMessageId(null);
    }
  }, [editingUserMessageId, messages, turnBusy]);

  /**
   * Live tool: only while a tool is running in this turn.
   * Completing a tool (or content resuming) clears it; next tool replaces.
   */
  const liveTool = useMemo(() => {
    if (!turnBusy) return null;
    return pickRunningTurnTool(messages);
  }, [messages, turnBusy]);

  /** Last assistant bubble after the latest user (anchor for mid-stream tool text). */
  const activeAssistantId = useMemo(() => {
    let lastUser = -1;
    for (let i = messages.length - 1; i >= 0; i--) {
      if (isTurnPromptMessage(messages[i])) {
        lastUser = i;
        break;
      }
    }
    let lastAssistantId: string | null = null;
    for (let i = lastUser + 1; i < messages.length; i++) {
      const m = messages[i]!;
      if (m.role === "assistant" && !m.isError) {
        lastAssistantId = m.id;
        if (m.streaming) return m.id;
      }
    }
    return turnBusy ? lastAssistantId : null;
  }, [messages, turnBusy]);

  const hasStreamingAssistant = messages.some(
    (m) => m.role === "assistant" && m.streaming,
  );
  const processingDurations = useMemo(
    () => processingDurationAnchors(messages),
    [messages],
  );
  const latestSubagentToolIds = useMemo(
    () =>
      latestSubagentToolCallIds(
        messages.flatMap((message) => messageSegments(message)),
        subagents,
      ),
    [messages, subagents],
  );

  // Quiet thinking only before this turn has an Assistant anchor.
  const showQuietThinking =
    turnBusy && !liveTool && !hasStreamingAssistant && !activeAssistantId;

  const empty =
    messages.length === 0 &&
    !showQuietThinking &&
    !liveTool &&
    !turnBusy;

  /**
   * 仅保留真实会渲染的消息，避免隐藏工具行占用虚拟高度并制造空白视口。
   * 原始 messages 仍用于工具编织、当前轮判断和附件路径解析。
   */
  const transcriptMessages = useMemo(
    () =>
      messages.filter((message) => {
        if (isToolStepMessage(message)) {
          const toolCallId =
            (message.toolCallId || "").trim() ||
            (message.id.startsWith("tool-") ? message.id.slice(5) : "");
          if (toolCallId && isToolInlinedInAssistants(messages, toolCallId)) {
            return false;
          }
          const toolSegment = toolSegmentFromMessage(message);
          // Todo/Goal 只投影到各自的状态面板。若把这些不会产生 DOM 的
          // tool_step 留在虚拟列表中，每项仍会贡献 flex gap；切换回运行中
          // 的长任务时，这些空行会累积成一大片白色占位。
          return !!toolSegment && !isComposerStateTool(toolSegment);
        }
        if (message.role !== "tool") return true;
        return (
          isEndOfTurnMarker(message.marker) ||
          message.marker === "turn_cancelled" ||
          message.marker === "context_compact" ||
          message.marker === "system_notification" ||
          message.content?.startsWith("turn_cancelled") ||
          message.content?.startsWith("turn_end|") ||
          message.content?.startsWith("context_compact") ||
          !!message.compactMeta
        );
      }),
    [messages],
  );

  /**
   * 强制保留查找目标和当前轮尾部；浏览历史时，虚拟列表会忽略距离过远的强制索引。
   */
  const forceVirtualIndices = useMemo(() => {
    const indices: number[] = [];
    const pushMessageId = (messageId: string | null | undefined) => {
      if (!messageId) return;
      const index = transcriptMessages.findIndex(
        (message) => message.id === messageId,
      );
      if (index >= 0) indices.push(index);
    };

    pushMessageId(findActive?.messageId);
    pushMessageId(activeAssistantId);

    if (turnBusy) {
      for (
        let index = Math.max(0, transcriptMessages.length - 2);
        index < transcriptMessages.length;
        index += 1
      ) {
        indices.push(index);
      }
    } else if (transcriptMessages.length > 0) {
      indices.push(transcriptMessages.length - 1);
    }

    return Array.from(new Set(indices));
  }, [
    transcriptMessages,
    findActive?.messageId,
    activeAssistantId,
    turnBusy,
  ]);

  /**
   * 在 DOM 首次测量前按正文、思考、附件和视频估算消息高度，降低滚动条跳变。
   */
  const getEstimateHeight = useCallback(
    (index: number) => {
      const message = transcriptMessages[index];
      if (!message) return 120;
      const body = message.content || "";
      const hasVideoCard =
        message.role === "assistant" &&
        (/\.(mp4|webm|mov|mkv)(\b|$)/i.test(body) ||
          body.includes("127.0.0.1"));
      return estimateChatRowHeight({
        contentLength: body.length,
        thoughtLength: message.thought?.length ?? 0,
        role: message.role,
        attachmentCount: message.attachments?.length ?? 0,
        hasVideoCard,
      });
    },
    [transcriptMessages],
  );

  const {
    virtualized,
    start: virtualStart,
    end: virtualEnd,
    paddingTop,
    paddingBottom,
    measureRef,
  } = useChatMessageVirtualizer({
    itemCount: transcriptMessages.length,
    getKey: (index) => transcriptMessages[index]?.id ?? `message-${index}`,
    getEstimateHeight,
    viewportRef: scrollRef,
    isPinnedRef,
    conversationKey: sessionKey ?? "chat",
    forceIndices: forceVirtualIndices,
  });

  /** 当前应挂载到 DOM 的消息窗口；短会话仍完整渲染。 */
  const visibleMessages = useMemo(() => {
    if (!virtualized) {
      return transcriptMessages.map((message, index) => ({ message, index }));
    }
    const windowed: Array<{ message: ChatMessage; index: number }> = [];
    for (let index = virtualStart; index < virtualEnd; index += 1) {
      const message = transcriptMessages[index];
      if (message) windowed.push({ message, index });
    }
    return windowed;
  }, [transcriptMessages, virtualized, virtualStart, virtualEnd]);

  return (
    <div ref={chatRootRef} className="lobe-chat" data-slot="lobe-chat">
      <div
        ref={scrollRef}
        className="lobe-chat__scroll"
      >
        <div ref={contentRef} className="lobe-chat__inner">
          {modelLabel ? (
            <div className="lobe-chat-model-divider" role="note">
              <span>{tr("chat.usingModel", { model: modelLabel })}</span>
            </div>
          ) : null}
          {empty && !suppressEmptyCopy ? (
            <div className="lobe-chat-empty">
              <h3 className="lobe-chat-empty__title">{tr("main.startTitle")}</h3>
              <p className="lobe-chat-empty__desc">{tr("main.startHint")}</p>
            </div>
          ) : null}

          {virtualized && paddingTop > 0 ? (
            <div
              aria-hidden
              className="lobe-chat__virt-spacer"
              style={{ height: paddingTop, flexShrink: 0 }}
            />
          ) : null}

          {visibleMessages.map(({ message: m, index: messageIndex }) => {
            /** 为虚拟消息行提供稳定测量容器，短会话保持原 DOM 层级。 */
            const wrap = (node: ReactNode) =>
              virtualized ? (
                <div
                  key={m.id}
                  ref={measureRef(messageIndex)}
                  data-virtual-message-index={messageIndex}
                >
                  {node}
                </div>
              ) : (
                node
              );

            if (
              isEndOfTurnMarker(m.marker) ||
              m.marker === "turn_cancelled" ||
              (m.role === "tool" &&
                (m.content?.startsWith("turn_cancelled") ||
                  m.content?.startsWith("turn_end|")))
            ) {
              return wrap(
                <EndOfTurnChip key={m.id} message={m} locale={locale} />
              );
            }

            // Standalone tool_step only when not already woven into an assistant
            // timeline (tools before first assistant bubble, edge cases).
            if (isToolStepMessage(m)) {
              const tcid =
                (m.toolCallId || "").trim() ||
                (m.id.startsWith("tool-") ? m.id.slice(5) : "");
              if (tcid && isToolInlinedInAssistants(messages, tcid)) {
                return null;
              }
              const toolSeg = toolSegmentFromMessage(m);
              if (!toolSeg) return null;
              if (isComposerStateTool(toolSeg)) return null;
              return wrap(
                <div key={m.id} className="lobe-chat-assistant-timeline">
                  <div className="lobe-timeline-rail">
                    <TimelineToolRow
                      tool={toolSeg}
                      locale={locale}
                      onOpenResource={onOpenResource}
                      subagents={subagents}
                    />
                  </div>
                </div>
              );
            }

            if (
              m.marker === "system_notification" &&
              m.role === "tool"
            ) {
              const level = m.systemNotificationLevel || "info";
              return wrap(
                <div
                  key={m.id}
                  className="lobe-chat-compact"
                  role={level === "error" ? "alert" : "status"}
                  data-level={level}
                  data-message-marker="system_notification"
                >
                  <span className="lobe-chat-compact__icon" aria-hidden>
                    <IconInfo size={15} />
                  </span>
                  <div className="lobe-chat-compact__body">
                    <div className="lobe-chat-compact__detail">
                      {localizeSystemNotification(m.content, locale)}
                    </div>
                  </div>
                </div>,
              );
            }

            if (
              m.marker === "context_compact" ||
              (m.role === "tool" &&
                (m.content?.startsWith("context_compact") ||
                  m.compactMeta))
            ) {
              const meta = m.compactMeta;
              const auto = (meta?.trigger || "auto") !== "manual";
              const title = auto
                ? tr("compact.bannerAuto")
                : tr("compact.bannerManual");
              let detail = "";
              if (
                meta?.tokensBefore != null &&
                meta?.tokensAfter != null &&
                Number.isFinite(meta.tokensBefore) &&
                Number.isFinite(meta.tokensAfter)
              ) {
                detail = tr("compact.tokensRange", {
                  before: formatTokenCount(meta.tokensBefore),
                  after: formatTokenCount(meta.tokensAfter),
                });
              } else if (meta?.note) {
                detail = meta.note;
              }
              const summary = meta?.summaryPreview?.trim();
              return wrap(
                <div
                  key={m.id}
                  className="lobe-chat-compact"
                  role="status"
                  data-trigger={meta?.trigger || "auto"}
                >
                  <span className="lobe-chat-compact__icon" aria-hidden>
                    <IconArrowsMinimize size={15} />
                  </span>
                  <div className="lobe-chat-compact__body">
                    <div className="lobe-chat-compact__title">{title}</div>
                    {detail ? (
                      <div className="lobe-chat-compact__detail">{detail}</div>
                    ) : null}
                    {summary ? (
                      <Collapsible className="lobe-chat-compact__summary">
                        <CollapsibleTrigger>
                          {tr("compact.summaryToggle")}
                        </CollapsibleTrigger>
                        <CollapsibleContent>
                          <p>{summary}</p>
                        </CollapsibleContent>
                      </Collapsible>
                    ) : null}
                  </div>
                </div>
              );
            }

            // Generic tool rows (non marker) — keep quiet; no history stack.
            if (m.role === "tool") {
              return null;
            }

            if (m.role === "user") {
              const timeLabel = formatMessageTime(m.createdAt, locale);
              const isFindHit = !!findHitMessageIds?.has(m.id);
              const isFindCurrent = findActive?.messageId === m.id;
              const isEditing = editingUserMessageId === m.id;
              const canEdit =
                !turnBusy &&
                m.id === lastUserMessageId &&
                !!m.content.trim() &&
                !!onEditLastUserMessage;
              return wrap(
                <ChatItem
                  key={m.id}
                  id={m.id}
                  placement="right"
                  showAvatar={false}
                  showTitle={false}
                  className={
                    (isFindHit ? " lobe-chat-item--find-hit" : "") +
                    (isFindCurrent ? " lobe-chat-item--find-current" : "")
                  }
                  message={
                    <div className="lobe-chat-user-stack">
                      {m.attachments && m.attachments.length > 0 ? (
                        <div className="lobe-chat-atts lobe-chat-atts--user">
                          {m.attachments.map((a) => (
                            <AttachmentCard
                              key={a.path}
                              attachment={a}
                              variant="card"
                              labels={attachLabels}
                              galleryPaths={m.attachments
                                ?.filter((x) => !x.isDir && isImagePath(x.path))
                                .map((x) => x.path)}
                              onAddToComposer={onAddAttachmentToComposer}
                            />
                          ))}
                        </div>
                      ) : null}
                      {isEditing && onEditLastUserMessage ? (
                        <UserMessageEditor
                          initialValue={m.content}
                          locale={locale}
                          onCancel={() => setEditingUserMessageId(null)}
                          onSend={(content) => onEditLastUserMessage(m, content)}
                        />
                      ) : m.content.trim() ? (
                        <div
                          className="lobe-chat-bubble"
                          data-message-marker={m.marker}
                        >
                          <UserPlainOrSkills
                            content={m.content}
                            findQuery={findQuery}
                            findActiveOccurrence={
                              isFindCurrent
                                ? (findActive?.occurrence ?? null)
                                : null
                            }
                          />
                        </div>
                      ) : null}
                    </div>
                  }
                  actions={
                    <>
                      {timeLabel ? (
                        <span className="lobe-chat-action-time">{timeLabel}</span>
                      ) : null}
                      {m.content.trim() ? (
                        <MessageCopyButton
                          text={m.content}
                          copyLabel={tr("message.copy")}
                          copiedLabel={tr("message.copied")}
                        />
                      ) : null}
                      {canEdit && !isEditing ? (
                        <MessageActionButton
                          label={tr("message.edit")}
                          onClick={() => setEditingUserMessageId(m.id)}
                        >
                          <IconRename size={15} />
                        </MessageActionButton>
                      ) : null}
                    </>
                  }
                />
              );
            }

            if (m.isError) {
              const friendly = m.errorBodyFormatted
                ? m.content
                : formatTurnErrorBody(
                    { content: m.content, code: undefined, message: undefined },
                    locale,
                  );
              const isFindHit = !!findHitMessageIds?.has(m.id);
              const isFindCurrent = findActive?.messageId === m.id;
              return wrap(
                <div
                  key={m.id}
                  className={
                    "lobe-chat-error" +
                    (isFindHit ? " lobe-chat-item--find-hit" : "") +
                    (isFindCurrent ? " lobe-chat-item--find-current" : "")
                  }
                  role="alert"
                  data-testid="chat-turn-error"
                  data-message-id={m.id}
                >
                  <div className="lobe-chat-error__label">
                    {tr("chat.turnFailed")}
                  </div>
                  <div className="lobe-chat-error__body">
                    {findQuery.trim() ? (
                      <HighlightedText
                        text={friendly}
                        query={findQuery}
                        activeOccurrence={
                          isFindCurrent
                            ? (findActive?.occurrence ?? null)
                            : null
                        }
                      />
                    ) : (
                      friendly
                    )}
                  </div>
                </div>
              );
            }

            // Assistant — thought / tool / body in true stream order.
            const segs = messageSegments(m);
            const isActiveAssistant = activeAssistantId === m.id;
            const hasInlinedRunningTool = segs.some(
              (s) => s.kind === "tool" && toolSegmentIsRunning(s),
            );
            // Fallback live line only when tool not yet woven into segments.
            const showLiveToolBelow =
              !!liveTool && isActiveAssistant && !hasInlinedRunningTool;
            const contentSegCount = segs.filter((s) => s.kind === "content")
              .length;
            let lastContentSi = -1;
            for (let i = segs.length - 1; i >= 0; i--) {
              if (segs[i]!.kind === "content") {
                lastContentSi = i;
                break;
              }
            }

            const isFindHit = !!findHitMessageIds?.has(m.id);
            const isFindCurrent = findActive?.messageId === m.id;
            // Phase projection: thought+tools collapse when phase ends (content
            // / next thought), not only when the full answer is done.
            const conversationSegs = segs.filter((segment) => {
              if (segment.kind === "tool") {
                return !isComposerStateTool(segment);
              }
              if (segment.kind === "thought" && !m.streaming) {
                return hasMeaningfulThinkingText(segment.text);
              }
              return true;
            });
            const timelineUnits = buildTimelineUnits(conversationSegs, {
              streaming: !!m.streaming,
            });
            /** 每个用户回合的首条 Assistant 回复顶部展示一次总工作耗时。 */
            const assistantBusy = turnBusy && isActiveAssistant;
            const processingDurationMs = processingDurations.get(m.id);
            const showProcessingTime =
              !!m.streaming || assistantBusy || processingDurationMs != null;
            const hasAssistantContent = !!m.content.trim();
            const showTurnMetrics =
              !m.streaming && hasDisplayableTurnMetrics(m.turnMetrics);
            const observedTurnId =
              m.turnMetrics?.turnId ?? (m.streaming ? activeTurnId : undefined);
            const observeVisibleToken =
              observedTurnId && (m.streaming || m.turnMetrics != null)
                ? onFirstVisibleToken
                : undefined;

            return wrap(
              <ChatItem
                key={m.id}
                id={m.id}
                placement="left"
                showAvatar={false}
                loading={!!m.streaming}
                className={
                  (isFindHit ? " lobe-chat-item--find-hit" : "") +
                  (isFindCurrent ? " lobe-chat-item--find-current" : "")
                }
                message={
                  <div
                    className="lobe-chat-assistant-timeline"
                    aria-busy={m.streaming ? true : undefined}
                    aria-live={m.streaming ? "polite" : undefined}
                    data-find-assistant={isFindCurrent ? "current" : undefined}
                  >
                    {showProcessingTime ? (
                      <Thinking
                        locale={locale}
                        thinking={!!m.streaming || assistantBusy}
                        startedAt={assistantBusy ? turnStartedAt : null}
                        durationMs={processingDurationMs}
                        statusLabel={(duration, running) =>
                          tr(running ? "chat.workingFor" : "chat.workedFor", {
                            duration,
                          })
                        }
                      />
                    ) : null}
                    {(() => {
                      // Running occurrence base across content segments so
                      // find marks stay aligned with message-level match index.
                      let contentOccBase = 0;
                      return timelineUnits.map((unit) => {
                        if (unit.kind === "phase") {
                          return (
                            <TimelinePhaseBlock
                              key={`${m.id}-${unit.id}`}
                              phase={unit}
                              locale={locale}
                              messageStreaming={!!m.streaming}
                              onOpenResource={onOpenResource}
                              onFirstVisibleToken={observeVisibleToken}
                              latencyTurnId={observedTurnId}
                              subagents={subagents}
                            />
                          );
                        }
                        if (unit.kind === "tool") {
                          const subagent = subagentForTool(
                            unit.tool,
                            subagents,
                          );
                          return (
                            <div
                              key={`${m.id}-tool-${unit.tool.toolCallId || unit.si}`}
                              className={
                                "lobe-timeline-rail lobe-timeline-rail--tool" +
                                (subagent
                                  ? " lobe-timeline-rail--subagent"
                                  : "")
                              }
                            >
                              <TimelineToolRow
                                tool={unit.tool}
                                locale={locale}
                                onOpenResource={onOpenResource}
                                subagents={subagents}
                                isLatestSubagentEvent={latestSubagentToolIds.has(
                                  unit.tool.toolCallId,
                                )}
                              />
                            </div>
                          );
                        }
                        if (unit.kind === "thought") {
                          if (!unit.text.trim()) return null;
                          return (
                            <div
                              key={`${m.id}-th-${unit.si}`}
                              className="lobe-timeline-rail"
                            >
                              <Thinking
                                locale={locale}
                                thinking={unit.streaming}
                                content={unit.text}
                                statusLabel={(duration, running) =>
                                  tr(running ? "chat.workingFor" : "chat.workedFor", {
                                    duration,
                                  })
                                }
                                onFirstVisibleToken={observeVisibleToken}
                                latencyTurnId={observedTurnId}
                              />
                            </div>
                          );
                        }
                        // content — never folded into a work phase
                        const segBase = contentOccBase;
                        if (findQuery.trim()) {
                          contentOccBase += findChatMatches(findQuery, [
                            {
                              id: `${m.id}-seg-${unit.si}`,
                              role: "assistant",
                              content: unit.text,
                            },
                          ]).length;
                        }
                        return (
                          <AssistantMessageBody
                            key={`${m.id}-c-${unit.si}`}
                            content={unit.text}
                            attachments={
                              unit.si === lastContentSi
                                ? m.attachments
                                : undefined
                            }
                            streaming={unit.streaming}
                            locale={locale}
                            projectPath={projectPath}
                            onOpenResource={onOpenResource}
                            onAddAttachmentToComposer={
                              onAddAttachmentToComposer
                            }
                            attachLabels={attachLabels}
                            findQuery={findQuery}
                            findActiveOccurrence={
                              isFindCurrent
                                ? (findActive?.occurrence ?? null)
                                : null
                            }
                            findOccurrenceBase={segBase}
                            onFirstVisibleToken={observeVisibleToken}
                            latencyTurnId={observedTurnId}
                          />
                        );
                      });
                    })()}
                    {/* Body-less turn with only attachments */}
                    {!contentSegCount && m.attachments?.length ? (
                      <AssistantMessageBody
                        content=""
                        attachments={m.attachments}
                        streaming={!!m.streaming}
                        locale={locale}
                        projectPath={projectPath}
                        onOpenResource={onOpenResource}
                        onAddAttachmentToComposer={onAddAttachmentToComposer}
                        attachLabels={attachLabels}
                        findQuery={findQuery}
                        findActiveOccurrence={
                          isFindCurrent
                            ? (findActive?.occurrence ?? null)
                            : null
                        }
                        onFirstVisibleToken={observeVisibleToken}
                        latencyTurnId={observedTurnId}
                      />
                    ) : null}
                  </div>
                }
                belowMessage={
                  showLiveToolBelow && liveTool ? (
                    <LiveToolText message={liveTool} locale={locale} />
                  ) : null
                }
                actions={
                  !m.streaming && (hasAssistantContent || showTurnMetrics) ? (
                    <>
                      {showTurnMetrics && m.turnMetrics ? (
                        <TurnMetrics summary={m.turnMetrics} locale={locale} />
                      ) : null}
                      {hasAssistantContent ? (
                        <MessageCopyButton
                          text={m.content}
                          copyLabel={tr("message.copy")}
                          copiedLabel={tr("message.copied")}
                        />
                      ) : null}
                    </>
                  ) : null
                }
                actionsOverlay={showTurnMetrics && !hasAssistantContent}
              />
            );
          })}

          {virtualized && paddingBottom > 0 ? (
            <div
              aria-hidden
              className="lobe-chat__virt-spacer"
              style={{ height: paddingBottom, flexShrink: 0 }}
            />
          ) : null}

          {/* Tool before any assistant bubble — only if not already a message row. */}
          {liveTool &&
          !activeAssistantId &&
          !(
            liveTool.toolCallId &&
            isToolInlinedInAssistants(messages, liveTool.toolCallId)
          ) &&
          !messages.some(
            (x) =>
              isToolStepMessage(x) &&
              (x.toolCallId === liveTool.toolCallId ||
                x.id === `tool-${liveTool.toolCallId}`),
          ) ? (
            <LiveToolText message={liveTool} locale={locale} />
          ) : null}

          {showQuietThinking ? (
            <div className="lobe-chat-live-tool is-running" role="status">
              <Thinking
                locale={locale}
                thinking
                startedAt={turnStartedAt}
                statusLabel={(duration, running) =>
                  tr(running ? "chat.workingFor" : "chat.workedFor", {
                    duration,
                  })
                }
              />
            </div>
          ) : null}

          {/* Stable live region for the current turn's retry state. */}
          <RetryStatus
            locale={locale}
            retryStatus={turnBusy ? retryStatus : null}
          />

          {/* Plan UI lives only in PlanStatusBar (top) + ResourceViewer Plan mode. */}
        </div>
      </div>

      <BackBottom
        visible={showBack}
        label={tr("chat.scrollBottom")}
        onClick={() => scrollToBottom("smooth")}
      />
    </div>
  );
}

import { useEffect, useState, type DragEvent as ReactDragEvent } from "react";
import type { Locale, MessageKey, Vars } from "@/i18n";
import type { QueuedSend } from "@/lib/sendQueue";
import type { SessionSnapshot } from "@/lib/session";
import type { SessionTurnResult } from "@/hooks/useSessionTurn";
import { Button } from "@appica/ui-react/button";
import { Textarea } from "@/components/ui/textarea";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  IconClock,
  IconArrowUp,
  IconArrowsSort,
  IconMore,
  IconRename,
  IconTrash,
} from "@/components/icons";
import { queuePreviewText } from "@/lib/sendQueue";
import { localizeUiError } from "@/lib/session";

type Translator = (key: MessageKey, vars?: Vars) => string;

/** 根据放置点计算“移动到目标前/后”的队列锚点。 */
function resolveDropAnchor(
  items: readonly QueuedSend[],
  sourceId: string,
  targetId: string,
  clientY: number,
  targetRect: DOMRect,
): string | null {
  if (sourceId === targetId || !items.some((item) => item.id === sourceId)) {
    return null;
  }
  const remaining = items.filter((item) => item.id !== sourceId);
  const targetIndex = remaining.findIndex((item) => item.id === targetId);
  if (targetIndex < 0) return null;

  const insertAfter = clientY >= targetRect.top + targetRect.height / 2;
  return remaining[insertAfter ? targetIndex + 1 : targetIndex]?.id ?? null;
}

export interface ComposerQueueProps {
  tr: Translator;
  locale: Locale;
  session: SessionSnapshot;
  sendQueue: SessionTurnResult["sendQueue"];
  queuePreviewLabels: {
    filesCount: (count: number) => string;
    empty: string;
  };
  steerQueuedItem: (item: QueuedSend) => Promise<void>;
  showToast: (message: string, duration?: number) => void;
}

export function ComposerQueue({
  tr,
  locale,
  session,
  sendQueue,
  queuePreviewLabels,
  steerQueuedItem,
  showToast,
}: ComposerQueueProps) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editingValue, setEditingValue] = useState("");
  const [draggedId, setDraggedId] = useState<string | null>(null);
  const [dropTargetId, setDropTargetId] = useState<string | null>(null);
  const cancelEditItem = sendQueue.cancelEditItem;

  useEffect(
    () => () => {
      if (editingId) cancelEditItem(editingId);
    },
    [cancelEditItem, editingId],
  );

  if (sendQueue.activeQueue.length === 0) return null;

  const closeEditor = () => {
    if (editingId) sendQueue.cancelEditItem(editingId);
    setEditingId(null);
    setEditingValue("");
  };

  const queueInteractionLocked =
    sendQueue.steeringIds.size > 0 || sendQueue.editingIds.size > 0;

  const clearDragState = () => {
    setDraggedId(null);
    setDropTargetId(null);
  };

  const handleDragStart = (
    event: ReactDragEvent<HTMLButtonElement>,
    itemId: string,
  ) => {
    if (queueInteractionLocked) {
      event.preventDefault();
      return;
    }
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", itemId);
    setDraggedId(itemId);
  };

  const handleDragOver = (
    event: ReactDragEvent<HTMLLIElement>,
    itemId: string,
  ) => {
    if (!draggedId || queueInteractionLocked || draggedId === itemId) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    setDropTargetId(itemId);
  };

  const handleDrop = (
    event: ReactDragEvent<HTMLLIElement>,
    targetId: string,
  ) => {
    event.preventDefault();
    const sourceId = draggedId ?? event.dataTransfer.getData("text/plain");
    if (sourceId && !queueInteractionLocked) {
      const beforeId = resolveDropAnchor(
        sendQueue.activeQueue,
        sourceId,
        targetId,
        event.clientY,
        event.currentTarget.getBoundingClientRect(),
      );
      if (sourceId !== targetId) sendQueue.reorderItem(sourceId, beforeId);
    }
    clearDragState();
  };

  const handleDragLeave = (event: ReactDragEvent<HTMLLIElement>) => {
    const nextTarget = event.relatedTarget;
    if (nextTarget && event.currentTarget.contains(nextTarget as Node)) return;
    setDropTargetId(null);
  };

  return (
    <div
      className="composer__queue"
      aria-label={tr("composer.queueCount", {
        n: String(sendQueue.activeQueue.length),
      })}
    >
      <div className="composer__queue-head">
        <IconClock size={14} aria-hidden />
        <span className="composer__queue-title">
          {tr("composer.queueCount", {
            n: String(sendQueue.activeQueue.length),
          })}
        </span>
        <Button size="md"
          type="button"
          variant="ghost"
          className="composer__queue-clear"
          disabled={
            sendQueue.steeringIds.size > 0 || sendQueue.editingIds.size > 0
          }
          onClick={sendQueue.clearQueue}
        >
          {tr("composer.queueClear")}
        </Button>
      </div>
      {sendQueue.flushHold ? (
        <div className="composer__queue-hold" role="status">
          <span className="composer__queue-hold-text">
            {tr("composer.queueHold")}
          </span>
          <Button size="md"
            type="button"
            variant="outline"
            className="composer__queue-hold-retry"
            onClick={sendQueue.resumeFlush}
          >
            {tr("composer.queueHoldRetry")}
          </Button>
        </div>
      ) : null}
      <ul className="composer__queue-list">
        {sendQueue.activeQueue.map((item, index) => (
          <li
            key={item.id}
            className={
              "composer__queue-item" +
              (draggedId === item.id ? " composer__queue-item--dragging" : "") +
              (dropTargetId === item.id ? " composer__queue-item--drop-target" : "")
            }
            onDragOver={(event) => handleDragOver(event, item.id)}
            onDrop={(event) => handleDrop(event, item.id)}
            onDragLeave={handleDragLeave}
          >
            {editingId === item.id ? (
              <div className="composer__queue-editor">
                <Textarea
                  inputSize="md"
                  autoFocus
                  value={editingValue}
                  aria-label={tr("message.editInput")}
                  onChange={(event) => setEditingValue(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Escape") closeEditor();
                    if (
                      event.key === "Enter" &&
                      (event.metaKey || event.ctrlKey)
                    ) {
                      event.preventDefault();
                      const next = editingValue.trim();
                      if (!next && item.attachments.length === 0) return;
                      sendQueue.updateItem(item.id, next);
                      setEditingId(null);
                    }
                  }}
                />
                <div className="composer__queue-editor-actions">
                  <Button
                    type="button"
                    variant="ghost" size="md"
                    onClick={closeEditor}
                  >
                    {tr("common.cancel")}
                  </Button>
                  <Button
                    type="button"
                    variant="primary" size="md"
                    disabled={
                      !editingValue.trim() && item.attachments.length === 0
                    }
                    onClick={() => {
                      sendQueue.updateItem(item.id, editingValue.trim());
                      setEditingId(null);
                    }}
                  >
                    {tr("common.save")}
                  </Button>
                </div>
              </div>
            ) : (
              <>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-md"
                  className="composer__queue-drag"
                  aria-label={tr("composer.queueDrag")}
                  title={tr("composer.queueDrag")}
                  draggable={!queueInteractionLocked}
                  disabled={queueInteractionLocked}
                  onDragStart={(event) => handleDragStart(event, item.id)}
                  onDragEnd={clearDragState}
                >
                  <IconArrowsSort size={14} />
                </Button>
                <span className="composer__queue-idx" aria-hidden>
                  {index + 1}
                </span>
                <span
                  className="composer__queue-text"
                  title={queuePreviewText(
                    item.storedDisplay,
                    item.attachments,
                    200,
                    queuePreviewLabels,
                  )}
                >
                  {queuePreviewText(
                    item.storedDisplay,
                    item.attachments,
                    72,
                    queuePreviewLabels,
                  )}
                </span>
                {session.state === "streaming" ? (
                  <Button
                    type="button"
                    variant="ghost"
                    size="md"
                    className="composer__queue-steer"
                    disabled={
                      queueInteractionLocked ||
                      sendQueue.steeringIds.has(item.id) ||
                      sendQueue.editingIds.has(item.id)
                    }
                    onClick={() => {
                      void sendQueue
                        .steerItem(item.id, steerQueuedItem)
                        .catch((error: unknown) =>
                          showToast(localizeUiError(error, locale), 4000),
                        );
                    }}
                  >
                    {sendQueue.steeringIds.has(item.id)
                      ? tr("composer.queueSteering")
                      : tr("composer.queueSteer")}
                  </Button>
                ) : (
                  <Button
                    type="button"
                    variant="secondary"
                    size="md"
                    className="composer__queue-send-now"
                    disabled={
                      session.state === "connecting" ||
                      queueInteractionLocked ||
                      sendQueue.steeringIds.has(item.id) ||
                      sendQueue.editingIds.has(item.id)
                    }
                    onClick={() => sendQueue.sendNowItem(item.id)}
                  >
                    <IconArrowUp size={14} />
                    {tr("composer.queueSendNow")}
                  </Button>
                )}
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-md"
                  className="composer__queue-remove"
                  aria-label={tr("composer.queueRemove")}
                  title={tr("composer.queueRemove")}
                  disabled={
                    sendQueue.steeringIds.has(item.id) ||
                    sendQueue.editingIds.has(item.id)
                  }
                  onClick={() => sendQueue.removeItem(item.id)}
                >
                  <IconTrash size={13} />
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger render={<Button
                      type="button"
                      variant="ghost"
                      size="icon-md"
                      className="composer__queue-more"
                      aria-label={tr("message.edit")}
                      title={tr("message.edit")}
                      disabled={
                        sendQueue.steeringIds.has(item.id) ||
                        sendQueue.editingIds.has(item.id)
                      }
                    />}>
                      <IconMore size={14} />
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem
                      onClick={() => {
                        sendQueue.beginEditItem(item.id);
                        setEditingId(item.id);
                        setEditingValue(item.storedDisplay);
                      }}
                    >
                      <IconRename size={15} />
                      {tr("message.editInput")}
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

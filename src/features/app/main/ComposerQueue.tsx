import { useEffect, useState } from "react";
import type { Locale, MessageKey, Vars } from "@/i18n";
import type { QueuedSend } from "@/lib/sendQueue";
import type { SessionSnapshot } from "@/lib/session";
import type { SessionTurnResult } from "@/hooks/useSessionTurn";
import { Button } from "@/components/ui/button";
import { Textarea } from "@appica/ui-react/textarea";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@appica/ui-react/dropdown-menu";
import {
  IconClock,
  IconMore,
  IconRename,
  IconTrash,
} from "@/components/icons";
import { queuePreviewText } from "@/lib/sendQueue";
import { localizeUiError } from "@/lib/session";

type Translator = (key: MessageKey, vars?: Vars) => string;

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
        <Button
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
          <Button
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
          <li key={item.id} className="composer__queue-item">
            {editingId === item.id ? (
              <div className="composer__queue-editor">
                <Textarea
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
                <Button
                  type="button"
                  variant="ghost"
                  className="composer__queue-steer"
                  disabled={
                    session.state !== "streaming" ||
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
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-md"
                  className="composer__queue-remove"
                  aria-label={tr("composer.queueRemove")}
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

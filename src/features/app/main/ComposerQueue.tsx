import type { Locale, MessageKey, Vars } from "@/i18n";
import type { QueuedSend } from "@/lib/sendQueue";
import type { SessionSnapshot } from "@/lib/session";
import type { SessionTurnResult } from "@/hooks/useSessionTurn";
import { Button } from "@/components/ui/button";
import { IconClock, IconClose } from "@/components/icons";
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
  if (sendQueue.activeQueue.length === 0) return null;

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
          className="composer__queue-clear"
          disabled={sendQueue.steeringIds.size > 0}
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
              className="composer__queue-steer"
              disabled={
                session.state !== "streaming" ||
                sendQueue.steeringIds.has(item.id)
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
              className="composer__queue-remove"
              aria-label={tr("composer.queueRemove")}
              disabled={sendQueue.steeringIds.has(item.id)}
              onClick={() => sendQueue.removeItem(item.id)}
            >
              <IconClose size={12} />
            </Button>
          </li>
        ))}
      </ul>
    </div>
  );
}

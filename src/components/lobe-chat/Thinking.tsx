import { Button } from "@/components/ui/button";
/**
 * Processing duration plus DeepSeek Harness-style reasoning disclosure.
 */

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { IconBrain, IconChevronDown } from "@/components/icons";
import { cn } from "@/lib/utils";
import { t, type Locale } from "@/i18n";

const useCommittedLayoutEffect =
  typeof window === "undefined" ? useEffect : useLayoutEffect;

/** 把处理耗时格式化为紧凑的分秒文本。 */
export function formatProcessingDuration(
  durationMs: number,
  locale: Locale,
): string {
  // 无效值与负值统一回落到 0；界面至少展示 1 秒。
  const safeDurationMs = Number.isFinite(durationMs)
    ? Math.max(0, durationMs)
    : 0;
  const totalSeconds = Math.max(1, Math.floor(safeDurationMs / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (locale === "zh" || locale === "zh-TW") {
    return minutes > 0 ? `${minutes}分${seconds}秒` : `${seconds}秒`;
  }
  return minutes > 0 ? `${minutes}m ${seconds}s` : `${seconds}s`;
}

/** DeepSeek Harness contract: live rows follow the latest line; settled rows show the first. */
export function reasoningSummary(text: string, running: boolean): string {
  if (!running) {
    const newline = text.indexOf("\n");
    return newline === -1 ? text : text.slice(0, newline);
  }
  const visible = text.trimEnd();
  const newline = visible.lastIndexOf("\n");
  return newline === -1 ? visible : visible.slice(newline + 1);
}

/** Keep the real streaming line aligned to its end without slicing its text. */
export function syncReasoningSummaryScroll(
  element: Pick<HTMLElement, "clientWidth" | "scrollLeft" | "scrollWidth">,
  running: boolean,
): void {
  element.scrollLeft = running
    ? Math.max(0, element.scrollWidth - element.clientWidth)
    : 0;
}

export function Thinking({
  content,
  thinking,
  durationMs,
  startedAt,
  processedLabel,
  locale = "en",
  onFirstVisibleToken,
  latencyTurnId,
}: {
  content?: string;
  /** 当前思考正文是否仍在流式生成。 */
  thinking?: boolean;
  /** Duration in ms (Lobe stores ms). */
  durationMs?: number;
  /** 本轮收到用户消息的时间戳。 */
  startedAt?: number | null;
  /** 例如“已处理 {duration}”。 */
  processedLabel: (duration: string) => string;
  locale?: Locale;
  onFirstVisibleToken?: (turnId: string) => void;
  latencyTurnId?: string;
}) {
  const [manuallyOpen, setManuallyOpen] = useState(false);
  const startRef = useRef<number | null>(startedAt ?? null);
  const [localDuration, setLocalDuration] = useState<number | undefined>(
    durationMs,
  );
  const summaryRef = useRef<HTMLSpanElement>(null);
  const firstVisibleCallbackRef = useRef(onFirstVisibleToken);
  firstVisibleCallbackRef.current = onFirstVisibleToken;
  const reportedVisibleKeyRef = useRef<string | null>(null);
  const hasBody = !!content?.trim();
  const tracksProcessingDuration = !hasBody;

  useEffect(() => {
    if (!tracksProcessingDuration) return;
    if (thinking) {
      if (startRef.current == null) startRef.current = startedAt ?? Date.now();
      if (startedAt != null && startedAt < startRef.current) {
        startRef.current = startedAt;
      }
      const updateDuration = () => {
        if (startRef.current != null) {
          setLocalDuration(Date.now() - startRef.current);
        }
      };
      updateDuration();
      const timer = window.setInterval(updateDuration, 1000);
      return () => window.clearInterval(timer);
    } else if (startRef.current != null) {
      setLocalDuration(durationMs ?? Date.now() - startRef.current);
      startRef.current = null;
    }
  }, [durationMs, startedAt, thinking, tracksProcessingDuration]);

  useEffect(() => {
    if (tracksProcessingDuration && durationMs != null) {
      setLocalDuration(durationMs);
    }
  }, [durationMs, tracksProcessingDuration]);

  const summary = hasBody ? reasoningSummary(content!, !!thinking) : "";
  const open = hasBody && manuallyOpen;

  useCommittedLayoutEffect(() => {
    const element = summaryRef.current;
    if (!element) return;
    syncReasoningSummaryScroll(element, !!thinking);
  }, [open, summary, thinking]);

  useCommittedLayoutEffect(() => {
    if (
      !hasBody ||
      !latencyTurnId ||
      reportedVisibleKeyRef.current === latencyTurnId
    ) {
      return;
    }
    reportedVisibleKeyRef.current = latencyTurnId;
    firstVisibleCallbackRef.current?.(latencyTurnId);
  }, [hasBody, latencyTurnId, summary]);

  /** Running and settled reasoning both remain under direct user control. */
  const toggle = () => {
    if (!hasBody) return;
    setManuallyOpen((value) => !value);
  };

  if (!hasBody) {
    return (
      <div className="lobe-chat-thinking" data-variant="processing">
        <div className="lobe-chat-thinking__trigger lobe-chat-thinking__trigger--status">
          <span
            className={cn(
              "lobe-chat-thinking__label",
              thinking && "lobe-chat-thinking__label--live",
            )}
          >
            {processedLabel(
              formatProcessingDuration(localDuration ?? 0, locale),
            )}
          </span>
        </div>
      </div>
    );
  }

  return (
    <div
      className="lobe-chat-thinking"
      data-variant="think"
      data-state={thinking ? "running" : "ok"}
    >
      {thinking ? (
        <span className="sr-only">{t(locale, "chat.thinking")}</span>
      ) : null}
      <Button
        type="button"
        className={cn("lobe-chat-thinking__trigger", open && "is-open")}
        aria-expanded={open}
        onClick={toggle}
      >
        <span className="lobe-chat-thinking__leading" aria-hidden>
          <IconBrain
            size={14}
            className="lobe-chat-thinking__icon lobe-chat-thinking__icon--idle"
          />
          <IconChevronDown
            size={14}
            className="lobe-chat-thinking__icon lobe-chat-thinking__icon--chevron"
          />
        </span>
        <span className="lobe-chat-thinking__title">
          {t(locale, "chat.thought")}
        </span>
        {!open ? (
          <>
            <span className="lobe-chat-thinking__separator" aria-hidden />
            <span
              ref={summaryRef}
              className="lobe-chat-thinking__summary"
              data-follow-end={thinking ? "true" : undefined}
            >
              {summary}
            </span>
          </>
        ) : null}
      </Button>
      {open ? <div className="lobe-chat-thinking__body">{content}</div> : null}
    </div>
  );
}

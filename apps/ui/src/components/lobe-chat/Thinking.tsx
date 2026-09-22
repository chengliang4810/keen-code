/**
 * KeenCode reasoning disclosure with processing duration.
 */

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@appica/ui-react/collapsible";
import { Button } from "@/components/ui/button";
import { IconBrain, IconChevronRight } from "@/components/icons";
import { cn } from "@/lib/utils";
import { t, type Locale } from "@/i18n";

const useCommittedLayoutEffect =
  typeof window === "undefined" ? useEffect : useLayoutEffect;

const REASONING_BOTTOM_LOCK_DISTANCE_PX = 2;
export const REASONING_CONTENT_UNLOAD_DELAY_MS = 300;

export type ReasoningScrollMaskState = "none" | "top" | "bottom" | "both";

export function getReasoningBottomDistance({
  clientHeight,
  scrollHeight,
  scrollTop,
}: {
  clientHeight: number;
  scrollHeight: number;
  scrollTop: number;
}): number {
  return Math.max(0, scrollHeight - clientHeight - scrollTop);
}

export function isReasoningScrollAtBottom(metrics: {
  clientHeight: number;
  scrollHeight: number;
  scrollTop: number;
}): boolean {
  return getReasoningBottomDistance(metrics) <= REASONING_BOTTOM_LOCK_DISTANCE_PX;
}

export function resolveReasoningScrollMaskState({
  clientHeight,
  scrollHeight,
  scrollTop,
}: {
  clientHeight: number;
  scrollHeight: number;
  scrollTop: number;
}): ReasoningScrollMaskState {
  const maxScrollTop = scrollHeight - clientHeight;
  if (maxScrollTop <= 1) return "none";

  const showTop = scrollTop > 1;
  const showBottom = scrollTop < maxScrollTop - 1;
  if (showTop && showBottom) return "both";
  if (showTop) return "top";
  if (showBottom) return "bottom";
  return "none";
}

const REASONING_VERTICAL_MASKS: Record<
  ReasoningScrollMaskState,
  string | undefined
> = {
  none: undefined,
  top: "linear-gradient(to bottom, transparent 0, var(--ink-black) 24px, var(--ink-black) 100%)",
  bottom:
    "linear-gradient(to bottom, var(--ink-black) 0, var(--ink-black) calc(100% - 24px), transparent 100%)",
  both:
    "linear-gradient(to bottom, transparent 0, var(--ink-black) 24px, var(--ink-black) calc(100% - 24px), transparent 100%)",
};

export function getReasoningScrollMaskStyle(
  state: ReasoningScrollMaskState,
): CSSProperties | undefined {
  const mask = REASONING_VERTICAL_MASKS[state];
  if (!mask) return undefined;
  return {
    WebkitMaskImage: mask,
    maskImage: mask,
    WebkitMaskRepeat: "no-repeat",
    maskRepeat: "no-repeat",
    WebkitMaskSize: "100% 100%",
    maskSize: "100% 100%",
  };
}

export function isReasoningSummaryOverflowing({
  clientWidth,
  scrollWidth,
}: {
  clientWidth: number;
  scrollWidth: number;
}): boolean {
  return scrollWidth > clientWidth + 1;
}

const REASONING_SUMMARY_MASK =
  "linear-gradient(to right, transparent 0, var(--ink-black) 16px, var(--ink-black) calc(100% - 16px), transparent 100%)";

export function getReasoningSummaryMaskStyle(
  isOverflowing: boolean,
): CSSProperties | undefined {
  if (!isOverflowing) return undefined;
  return {
    WebkitMaskImage: REASONING_SUMMARY_MASK,
    maskImage: REASONING_SUMMARY_MASK,
    WebkitMaskRepeat: "no-repeat",
    maskRepeat: "no-repeat",
    WebkitMaskSize: "100% 100%",
    maskSize: "100% 100%",
  };
}

/** 把处理耗时格式化为紧凑的天/小时/分秒文本。 */
export function formatProcessingDuration(
  durationMs: number,
  locale: Locale,
): string {
  // 无效值与负值统一回落到 0；界面至少展示 1 秒。
  const safeDurationMs = Number.isFinite(durationMs)
    ? Math.max(0, durationMs)
    : 0;
  const totalSeconds = Math.max(1, Math.floor(safeDurationMs / 1000));
  const days = Math.floor(totalSeconds / 86_400);
  const hours = Math.floor((totalSeconds % 86_400) / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  if (locale === "zh" || locale === "zh-TW") {
    const hourUnit = locale === "zh-TW" ? "小時" : "小时";
    const minuteUnit = locale === "zh-TW" ? "分鐘" : "分钟";
    if (days > 0) return hours > 0 ? `${days}天 ${hours}${hourUnit}` : `${days}天`;
    if (hours > 0) return minutes > 0 ? `${hours}${hourUnit} ${minutes}${minuteUnit}` : `${hours}${hourUnit}`;
    return minutes > 0
      ? `${minutes}${minuteUnit} ${seconds}秒`
      : `${seconds}秒`;
  }
  if (days > 0) return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  if (hours > 0) return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
  return minutes > 0 ? `${minutes}m ${seconds}s` : `${seconds}s`;
}

/** KeenCode summary contract: live rows follow the latest line; settled rows show the first. */
export function reasoningSummary(text: string, running: boolean): string {
  if (!running) {
    const normalized = text.replace(/\r\n?/gu, "\n");
    const newline = normalized.indexOf("\n");
    return newline === -1 ? normalized : normalized.slice(0, newline);
  }

  return resolveReasoningStreamingSummary(text);
}

/** 与 ZCode reasoning 保持一致：流式摘要取最后一个非空、去首尾空白的完整行。 */
export function resolveReasoningStreamingSummary(text: string): string {
  const lines = text.replace(/\r\n?/gu, "\n").split("\n");
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    const line = lines[index]?.trim() ?? "";
    if (line.length > 0) return line;
  }
  return "";
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
  statusLabel,
  locale = "en",
  onFirstVisibleToken,
  latencyTurnId,
}: {
  content?: string;
  /** 当前思考正文是否仍在流式生成。 */
  thinking?: boolean;
  /** Reasoning duration in milliseconds supplied by the runtime. */
  durationMs?: number;
  /** 本轮收到用户消息的时间戳。 */
  startedAt?: number | null;
  /** 根据运行状态生成“工作中/已工作”耗时文本。 */
  statusLabel: (duration: string, running: boolean) => string;
  locale?: Locale;
  onFirstVisibleToken?: (turnId: string) => void;
  latencyTurnId?: string;
}) {
  const [manuallyOpen, setManuallyOpen] = useState(false);
  const [shouldRenderContent, setShouldRenderContent] = useState(false);
  const [scrollMaskState, setScrollMaskState] =
    useState<ReasoningScrollMaskState>("none");
  const [summaryOverflowing, setSummaryOverflowing] = useState(false);
  const startRef = useRef<number | null>(startedAt ?? null);
  const [localDuration, setLocalDuration] = useState<number | undefined>(
    durationMs,
  );
  const summaryRef = useRef<HTMLSpanElement>(null);
  const summaryTextRef = useRef<HTMLSpanElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const autoFollowBottomRef = useRef(true);
  const firstVisibleCallbackRef = useRef(onFirstVisibleToken);
  firstVisibleCallbackRef.current = onFirstVisibleToken;
  const reportedVisibleKeyRef = useRef<string | null>(null);
  const hasBody = !!content?.trim();
  const tracksProcessingDuration = startedAt != null || durationMs != null;
  const summary = hasBody ? reasoningSummary(content!, !!thinking) : "";
  const open = hasBody && manuallyOpen;

  useEffect(() => {
    if (!tracksProcessingDuration) return;
    if (thinking) {
      if (startRef.current == null) startRef.current = startedAt ?? Date.now();
      if (startedAt != null && startedAt < startRef.current) {
        startRef.current = startedAt;
      }
      // 折叠的 reasoning 不需要每秒触发父级重绘；打开时仍从同一锚点补齐耗时。
      if (!open) return;
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
  }, [durationMs, startedAt, thinking, open, tracksProcessingDuration]);

  useEffect(() => {
    if (tracksProcessingDuration && durationMs != null) {
      setLocalDuration(durationMs);
    }
  }, [durationMs, tracksProcessingDuration]);

  useEffect(() => {
    if (!open) {
      autoFollowBottomRef.current = true;
      setScrollMaskState("none");
    }
  }, [open]);

  useEffect(() => {
    if (open) {
      setShouldRenderContent(true);
      return;
    }
    if (!shouldRenderContent) return;
    // Reduced motion 没有可等待的高度过渡，关闭时直接释放正文。
    if (window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) {
      setShouldRenderContent(false);
      return;
    }
    // WebView2 在隐藏面板或中断 transition 时可能不派发 transitionend；
    // 兜底卸载避免 keepMounted 正文长期占用 DOM 与 ResizeObserver。
    const unloadTimer = window.setTimeout(() => {
      setShouldRenderContent(false);
    }, REASONING_CONTENT_UNLOAD_DELAY_MS);
    return () => window.clearTimeout(unloadTimer);
  }, [open, shouldRenderContent]);

  useEffect(() => {
    if (!hasBody) setManuallyOpen(false);
  }, [hasBody]);

  useCommittedLayoutEffect(() => {
    const element = summaryRef.current;
    if (!element) {
      setSummaryOverflowing(false);
      return;
    }
    setSummaryOverflowing(isReasoningSummaryOverflowing(element));
    syncReasoningSummaryScroll(element, !!thinking);
  }, [open, summary, thinking]);

  useEffect(() => {
    const element = summaryRef.current;
    if (!element) return;

    const updateSummaryViewport = () => {
      setSummaryOverflowing((current) => {
        const next = isReasoningSummaryOverflowing(element);
        return current === next ? current : next;
      });
      syncReasoningSummaryScroll(element, !!thinking);
    };

    updateSummaryViewport();
    if (typeof ResizeObserver === "undefined") return;
    const resizeObserver = new ResizeObserver(updateSummaryViewport);
    resizeObserver.observe(element);
    if (summaryTextRef.current) resizeObserver.observe(summaryTextRef.current);
    return () => resizeObserver.disconnect();
  }, [open, summary, thinking]);

  const updateScrollMaskState = useCallback(() => {
    const element = scrollRef.current;
    if (!element) {
      setScrollMaskState("none");
      return;
    }
    const next = resolveReasoningScrollMaskState(element);
    setScrollMaskState((current) => (current === next ? current : next));
  }, []);

  const scrollToReasoningBottom = useCallback(() => {
    const element = scrollRef.current;
    if (!element) return;
    element.scrollTop = element.scrollHeight;
    updateScrollMaskState();
  }, [updateScrollMaskState]);

  const handleReasoningScroll = useCallback(() => {
    const element = scrollRef.current;
    if (!element) return;
    autoFollowBottomRef.current = isReasoningScrollAtBottom(element);
    updateScrollMaskState();
  }, [updateScrollMaskState]);

  useCommittedLayoutEffect(() => {
    if (!open || !shouldRenderContent) {
      setScrollMaskState("none");
      return;
    }
    if (autoFollowBottomRef.current) {
      scrollToReasoningBottom();
    } else {
      updateScrollMaskState();
    }
  }, [
    content,
    open,
    shouldRenderContent,
    scrollToReasoningBottom,
    updateScrollMaskState,
  ]);

  useEffect(() => {
    if (!open || !shouldRenderContent) return;
    const scrollNode = scrollRef.current;
    if (!scrollNode) return;

    const syncScrollPosition = () => {
      if (autoFollowBottomRef.current) {
        scrollToReasoningBottom();
      } else {
        updateScrollMaskState();
      }
    };

    syncScrollPosition();
    window.addEventListener("resize", syncScrollPosition);
    if (typeof ResizeObserver === "undefined") {
      return () => window.removeEventListener("resize", syncScrollPosition);
    }

    const resizeObserver = new ResizeObserver(syncScrollPosition);
    resizeObserver.observe(scrollNode);
    if (contentRef.current) resizeObserver.observe(contentRef.current);
    return () => {
      resizeObserver.disconnect();
      window.removeEventListener("resize", syncScrollPosition);
    };
  }, [open, shouldRenderContent, scrollToReasoningBottom, updateScrollMaskState]);

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

  const handleOpenChange = useCallback((nextOpen: boolean) => {
    setManuallyOpen(nextOpen);
    if (nextOpen) {
      autoFollowBottomRef.current = true;
      setShouldRenderContent(true);
    }
  }, []);

  if (!hasBody) {
    return (
      <div className="lobe-chat-thinking" data-variant="processing">
        <div className="lobe-chat-thinking__trigger lobe-chat-thinking__trigger--status">
          <span
            className={cn(
              "lobe-chat-thinking__label",
              thinking && "lobe-chat-thinking__label--live",
              thinking && "animated-gradient-text",
            )}
            data-reasoning-label="true"
          >
            {statusLabel(
              formatProcessingDuration(localDuration ?? 0, locale),
              !!thinking,
            )}
          </span>
        </div>
      </div>
    );
  }

  return (
    <Collapsible
      open={open}
      onOpenChange={handleOpenChange}
      className="lobe-chat-thinking"
      data-variant="think"
      data-state={thinking ? "running" : "ok"}
    >
      {thinking ? (
        <span className="sr-only">{t(locale, "chat.thinking")}</span>
      ) : null}
      <CollapsibleTrigger
        render={
          <Button
            type="button"
            variant="ghost"
            className={cn("lobe-chat-thinking__trigger", open && "is-open")}
          />
        }
      >
        <span className="lobe-chat-thinking__leading" aria-hidden>
          <IconBrain size={16} className="lobe-chat-thinking__icon" />
        </span>
        <span className="lobe-chat-thinking__title">
          <span
            className={cn(
              "lobe-chat-thinking__title-text",
              thinking && "animated-gradient-text",
            )}
            data-reasoning-label="true"
          >
            {thinking ? t(locale, "chat.thinking") : t(locale, "chat.thinkingProcess")}
          </span>
        </span>
        {!thinking && tracksProcessingDuration ? (
          <>
            <span className="lobe-chat-thinking__separator" aria-hidden />
            <span className="lobe-chat-thinking__duration">
              {t(locale, "chat.lastedFor").replace(
                "{duration}",
                formatProcessingDuration(localDuration ?? 0, locale),
              )}
            </span>
          </>
        ) : null}
        {thinking && !open && hasBody ? (
          <>
            <span className="lobe-chat-thinking__separator" aria-hidden />
            <span
              ref={summaryRef}
              className="lobe-chat-thinking__summary"
              data-follow-end={thinking ? "true" : undefined}
              data-summary-overflowing={summaryOverflowing ? "true" : "false"}
              data-reasoning-streaming-line={thinking ? "true" : undefined}
              data-reasoning-streaming-roll={thinking ? "true" : undefined}
              style={getReasoningSummaryMaskStyle(summaryOverflowing)}
            >
              <span
                ref={summaryTextRef}
                className="lobe-chat-thinking__summary-text"
                data-reasoning-streaming-text={thinking ? "true" : undefined}
              >
                {summary}
              </span>
            </span>
          </>
        ) : null}
        <span className="lobe-chat-thinking__caret" aria-hidden>
          <IconChevronRight size={16} />
        </span>
      </CollapsibleTrigger>
      <CollapsibleContent
        keepMounted
        className="lobe-chat-thinking__content"
        onTransitionEnd={(event) => {
          if (
            !open &&
            event.currentTarget === event.target &&
            event.propertyName === "height"
          ) {
            setShouldRenderContent(false);
          }
        }}
      >
        {shouldRenderContent ? (
          <div className="lobe-chat-thinking__content-inner">
            <div
              ref={scrollRef}
              className="lobe-chat-thinking__body"
              data-reasoning-scroll-mask={scrollMaskState}
              data-scroll-mask={scrollMaskState}
              onScroll={handleReasoningScroll}
              style={getReasoningScrollMaskStyle(scrollMaskState)}
            >
              <div ref={contentRef}>{content}</div>
            </div>
          </div>
        ) : null}
      </CollapsibleContent>
    </Collapsible>
  );
}

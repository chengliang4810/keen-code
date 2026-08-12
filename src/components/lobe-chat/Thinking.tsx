/**
 * Lobe Thinking — collapsible reasoning row.
 *
 * 处理期间从用户消息发送时开始计时并实时展开正文；处理完成后自动折叠。
 */

import { useEffect, useRef, useState, type ReactNode } from "react";
import { IconChevronDown } from "@/components/icons";
import { cn } from "@/lib/utils";
import { MarkdownChat } from "./MarkdownChat";
import type { Locale } from "@/i18n";

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

export function Thinking({
  content,
  thinking,
  durationMs,
  startedAt,
  processedLabel,
  triggerLabel,
  locale = "en",
}: {
  content?: string | ReactNode;
  /** 当前思考正文是否仍在流式生成。 */
  thinking?: boolean;
  /** Duration in ms (Lobe stores ms). */
  durationMs?: number;
  /** 本轮收到用户消息的时间戳。 */
  startedAt?: number | null;
  /** 例如“已处理 {duration}”。 */
  processedLabel: (duration: string) => string;
  /** 非首段思考使用的摘要标题；传入后不再显示或计算处理耗时。 */
  triggerLabel?: string;
  locale?: Locale;
}) {
  const [manuallyOpen, setManuallyOpen] = useState(false);
  const startRef = useRef<number | null>(startedAt ?? null);
  const [localDuration, setLocalDuration] = useState<number | undefined>(
    durationMs,
  );
  const tracksProcessingDuration = triggerLabel == null;

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
    if (!thinking) setManuallyOpen(false);
  }, [thinking]);

  useEffect(() => {
    if (tracksProcessingDuration && durationMs != null) {
      setLocalDuration(durationMs);
    }
  }, [durationMs, tracksProcessingDuration]);

  const resolvedTriggerLabel =
    triggerLabel ??
    processedLabel(formatProcessingDuration(localDuration ?? 0, locale));

  const hasBody =
    (typeof content === "string" && content.trim().length > 0) ||
    (content != null && typeof content !== "string");

  const open = !!thinking || manuallyOpen;

  /** 完成后允许用户手动查看思考正文；处理中始终保持展开。 */
  const toggle = () => {
    if (thinking) return;
    setManuallyOpen((value) => !value);
  };

  return (
    <div className="lobe-chat-thinking">
      <button
        type="button"
        className="lobe-chat-thinking__trigger"
        aria-expanded={open}
        onClick={toggle}
      >
        <span
          className={cn(
            "lobe-chat-thinking__label",
            thinking && "lobe-chat-thinking__label--live",
          )}
          style={{ color: "var(--lobe-color-text-secondary)" }}
        >
          {resolvedTriggerLabel}
        </span>
        {hasBody ? (
          <IconChevronDown
            size={12}
            className={cn(
              "lobe-chat-thinking__caret text-[var(--lobe-color-text-tertiary)] transition-transform shrink-0 ml-auto",
              open && "rotate-180",
            )}
          />
        ) : null}
      </button>
      {open && hasBody ? (
        <div className="lobe-chat-thinking__body">
          {typeof content === "string" ? (
            <MarkdownChat locale={locale} muted>
              {content}
            </MarkdownChat>
          ) : (
            content
          )}
        </div>
      ) : null}
    </div>
  );
}

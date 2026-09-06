/**
 * Completed-turn latency evidence.
 *
 * The row is mounted inside the existing assistant hover footer so metrics do
 * not add permanent visual noise or change the message body layout.
 */

import { useMemo } from "react";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import type { TurnLatencySummary } from "@/lib/turnLatency";

/** Format a latency value for the compact message footer. */
export function formatTurnLatency(durationMs: number): string | null {
  if (!Number.isFinite(durationMs) || durationMs < 0) return null;
  if (durationMs < 1_000) return `${Math.round(durationMs)}ms`;
  if (durationMs < 60_000) {
    const seconds = durationMs / 1_000;
    const precision = seconds < 10 ? 2 : 1;
    return `${Number(seconds.toFixed(precision))}s`;
  }
  const minutes = Math.floor(durationMs / 60_000);
  const seconds = Math.floor((durationMs % 60_000) / 1_000);
  return `${minutes}m ${seconds}s`;
}

/** Whether the footer has at least one honest, displayable observation. */
export function hasDisplayableTurnMetrics(
  summary: TurnLatencySummary | null | undefined,
): boolean {
  if (!summary) return false;
  return (
    formatTurnLatency(summary.timeToFirstTokenMs ?? Number.NaN) != null ||
    formatTurnLatency(summary.totalMs ?? Number.NaN) != null
  );
}

export function TurnMetrics({
  summary,
  locale,
}: {
  summary: TurnLatencySummary;
  locale: Locale;
}) {
  const tr = useMemo(() => createT(locale), [locale]);
  const text = useMemo(() => {
    const metrics: string[] = [];
    const appendDuration = (
      key:
        | "chat.turnMetrics.firstToken"
        | "chat.turnMetrics.completed",
      value: number | null,
    ) => {
      const formatted = formatTurnLatency(value ?? Number.NaN);
      if (formatted != null) metrics.push(tr(key, { value: formatted }));
    };

    appendDuration(
      "chat.turnMetrics.firstToken",
      summary.timeToFirstTokenMs,
    );
    appendDuration("chat.turnMetrics.completed", summary.totalMs);

    return metrics.join(" · ");
  }, [summary, tr]);

  if (!text) return null;

  return (
    <span
      className="lobe-turn-metrics"
      title={text}
      aria-label={`${tr("chat.turnMetrics.label")}: ${text}`}
      data-testid="turn-metrics"
      tabIndex={0}
    >
      {text}
    </span>
  );
}

/** 每轮尾部的用量与用时入口；数值来自权威 Journal，缺失证据保持未知。 */
import { Fragment } from "react";
import { createT, type Locale } from "@/i18n";
import { IconClock, IconDatabase } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import type { TurnLatencySummary } from "@/lib/turnLatency";
import { formatMetricTokens, formatTurnLatency, formatRunDuration, formatTokensPerSecond } from "@/lib/turnMetricsPresentation";
import "@/styles/turn-metrics.css";

export function TurnMetrics({ summary, locale, durationMs }: {
  summary?: TurnLatencySummary;
  locale: Locale;
  /** 无权威用量的历史消息仍可显示已有的持久化工作用时。 */
  durationMs?: number;
}) {
  const tr = createT(locale);
  const unknown = tr("chat.turnMetrics.unknown");
  const total = summary?.totalTokens;
  const runMs = summary?.totalMs ?? durationMs;
  const runLabel = formatRunDuration(runMs, locale) ?? unknown;
  const tokenLabel = (value: number | null | undefined) => formatMetricTokens(value, locale, false) ?? unknown;
  const usageRows = [
    ["chat.turnMetrics.input", summary?.inputTokens],
    ["chat.turnMetrics.output", summary?.outputTokens],
    ["chat.turnMetrics.reasoning", summary?.reasoningTokens],
    ["chat.turnMetrics.cacheRead", summary?.cacheReadTokens],
  ] as const;

  return (
    <span className="lobe-turn-metrics" data-testid="turn-metrics">
      <Popover>
        <span className="ui-stat-root">
          <PopoverTrigger asChild>
            <Button variant="stat" aria-label={tr("chat.turnMetrics.usage")}>
              <IconDatabase size={15} />
              <span className="ui-stat-label">{tr("chat.turnMetrics.usageValue", {
                value: formatMetricTokens(total, locale, true) ?? "—",
              })}</span>
            </Button>
          </PopoverTrigger>
        </span>
        <PopoverContent side="top" aria-label={tr("chat.turnMetrics.usage")}>
          <div className="ui-stat-title">
            <span className="ui-stat-title-label"><IconDatabase size={14} />{tr("chat.turnMetrics.usage")}</span>
            <span>{tokenLabel(total)}</span>
          </div>
          <div className="ui-stat-rule" />
          <dl className="ui-stat-details">
            {usageRows.map(([key, value]) => (
              <Fragment key={key}><dt>{tr(key)}</dt><dd>{tokenLabel(value)}</dd></Fragment>
            ))}
          </dl>
          {total == null && <p className="ui-stat-note">{tr(summary ? "chat.turnMetrics.noUsage" : "chat.turnMetrics.notObserved")}</p>}
        </PopoverContent>
      </Popover>
      <Popover>
        <span className="ui-stat-root">
          <PopoverTrigger asChild>
            <Button variant="stat" aria-label={tr("chat.turnMetrics.time")}>
              <IconClock size={15} />
              <span className="ui-stat-label">{tr("chat.turnMetrics.timeValue", { value: runMs == null ? "—" : runLabel })}</span>
            </Button>
          </PopoverTrigger>
        </span>
        <PopoverContent side="top" aria-label={tr("chat.turnMetrics.time")}>
          <div className="ui-stat-title">
            <span className="ui-stat-title-label"><IconClock size={14} />{tr("chat.turnMetrics.time")}</span>
          </div>
          <div className="ui-stat-rule" />
          <dl className="ui-stat-details">
            <dt>{tr("chat.turnMetrics.totalTime")}</dt><dd>{runLabel}</dd>
            <dt>{tr("chat.turnMetrics.outputSpeed")}</dt>
            <dd>{formatTokensPerSecond(summary?.tokensPerSecond) != null
              ? `${formatTokensPerSecond(summary?.tokensPerSecond)} tokens/s` : unknown}</dd>
            <dt>{tr("chat.turnMetrics.firstTokenLabel")}</dt>
            <dd>{formatTurnLatency(summary?.timeToFirstTokenMs ?? Number.NaN) ?? unknown}</dd>
          </dl>
        </PopoverContent>
      </Popover>
    </span>
  );
}

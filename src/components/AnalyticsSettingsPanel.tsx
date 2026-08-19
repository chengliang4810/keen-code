import { useEffect, useMemo, useState } from "react";
import {
  usageStatsGet,
  type DailyUsageStat,
  type UsageStats,
} from "@/lib/api";
import { formatTokenCount } from "@/lib/contextUsage";

const TREND_DAY_COUNT = 31;
const MAX_TREND_TICKS = 7;

type Props = {
  labels: {
    loading: string;
    empty: string;
    totalRequests: string;
    totalTokens: string;
    byModel: string;
    byDay: string;
    activityHeatmap: string;
    less: string;
    more: string;
    tokenTrend: string;
    modelUsage: string;
    rounds: string;
  };
};

const MODEL_COLORS = ["#1683f8", "#209447", "#9567ec", "#ed3434", "#e78000", "#0ba6a6", "#e85ca3", "#64748b"];

export function localDateKey(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function startOfLocalDay(value: Date): Date {
  const date = new Date(value);
  date.setHours(0, 0, 0, 0);
  return date;
}

export function recentDays(count: number, today = new Date()): Date[] {
  const dayCount = Math.max(0, Math.floor(count));
  const end = startOfLocalDay(today);
  return Array.from({ length: dayCount }, (_, index) => {
    const date = new Date(end);
    date.setDate(end.getDate() - (dayCount - index - 1));
    return date;
  });
}

export type AnalyticsTrendDay = {
  date: Date;
  dateKey: string;
  stat: DailyUsageStat | undefined;
};

/**
 * Build one ordered local-calendar sequence for both trend bars and labels.
 * The backend dates are already local `YYYY-MM-DD` values; matching by the
 * same key avoids parsing them as UTC and keeps missing calendar days aligned.
 */
export function buildAnalyticsTrendDays(
  days: DailyUsageStat[],
  count = TREND_DAY_COUNT,
  today = new Date(),
): AnalyticsTrendDay[] {
  const daysByDate = new Map(days.map((item) => [item.date, item]));
  return recentDays(count, today).map((date) => {
    const dateKey = localDateKey(date);
    return { date, dateKey, stat: daysByDate.get(dateKey) };
  });
}

/** Return evenly spaced, readable date tick positions for the trend axis. */
export function analyticsTrendTickIndexes(
  dayCount: number,
  maxTicks = MAX_TREND_TICKS,
): number[] {
  const count = Math.max(0, Math.floor(dayCount));
  const limit = Math.max(1, Math.floor(maxTicks));
  if (count === 0) return [];
  if (count <= limit) return Array.from({ length: count }, (_, index) => index);
  if (limit === 1) return [0];
  return Array.from({ length: limit }, (_, index) =>
    Math.round((index * (count - 1)) / (limit - 1)),
  );
}

export function formatAnalyticsTrendDate(date: Date): string {
  return `${date.getMonth() + 1}/${date.getDate()}`;
}

export function analyticsModelPercent(tokens: number, totalTokens: number): number {
  if (!Number.isFinite(tokens) || !Number.isFinite(totalTokens) || totalTokens <= 0) {
    return 0;
  }
  return Math.max(0, tokens) / totalTokens;
}

export function AnalyticsSettingsPanel({ labels }: Props) {
  const [stats, setStats] = useState<UsageStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    void usageStatsGet()
      .then((result) => {
        if (!active) return;
        setStats(result);
      })
      .catch((cause) => {
        if (active) setError(cause instanceof Error ? cause.message : String(cause));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, []);

  const usageView = useMemo(() => {
    if (!stats) return null;
    const daysByDate = new Map(stats.days.map((item) => [item.date, item]));
    const heatmapDays = recentDays(364).map((date) => ({
      date,
      stat: daysByDate.get(localDateKey(date)),
    }));
    const trendDays = buildAnalyticsTrendDays(stats.days);
    const maxDayTokens = Math.max(1, ...trendDays.map((item) => item.stat?.totalTokens ?? 0));
    const maxHeatTokens = Math.max(1, ...heatmapDays.map((item) => item.stat?.totalTokens ?? 0));
    const models = [...stats.models].sort((a, b) => b.totalTokens - a.totalTokens);
    const trendTickIndexes = analyticsTrendTickIndexes(trendDays.length);
    return { heatmapDays, trendDays, trendTickIndexes, maxDayTokens, maxHeatTokens, models };
  }, [stats]);

  if (loading) return <div className="analytics-empty">{labels.loading}</div>;
  if (error) return <div className="analytics-empty is-error">{error}</div>;

  if (!stats || !stats.totalRequests || !usageView) return <div className="analytics-empty">{labels.empty}</div>;
  let donutOffset = 0;
  return (
    <div className="analytics-dashboard">
      <div className="analytics-kpis">
        <div className="analytics-kpi"><span>{labels.totalRequests}</span><strong>{stats.totalRequests.toLocaleString()}</strong></div>
        <div className="analytics-kpi"><span>{labels.totalTokens}</span><strong>{formatTokenCount(stats.totalTokens)}</strong></div>
      </div>
      <section className="analytics-chart-card">
        <div className="analytics-chart-heading">
          <h3>{labels.activityHeatmap}</h3>
          <div className="analytics-heat-legend"><span>{labels.less}</span>{[0, 1, 2, 3, 4].map((level) => <i key={level} data-level={level} />)}<span>{labels.more}</span></div>
        </div>
        <div className="analytics-heatmap" role="img" aria-label={labels.activityHeatmap}>
          {usageView.heatmapDays.map(({ date, stat }) => {
            const tokens = stat?.totalTokens ?? 0;
            const level = tokens === 0 ? 0 : Math.min(4, Math.max(1, Math.ceil((tokens / usageView.maxHeatTokens) * 4)));
            return (
              <i
                key={localDateKey(date)}
                data-level={level}
                title={`${date.toLocaleDateString()}: ${tokens.toLocaleString()} Tokens · ${stat?.requests ?? 0} ${labels.rounds}`}
              />
            );
          })}
        </div>
      </section>
      <section className="analytics-chart-card">
        <h3>{labels.tokenTrend}</h3>
        <div className="analytics-token-trend" role="img" aria-label={labels.tokenTrend}>
          <div className="analytics-trend-bars">
            {usageView.trendDays.map(({ date, dateKey, stat }) => (
              <div className="analytics-trend-day" key={dateKey} title={`${date.toLocaleDateString()}: ${(stat?.totalTokens ?? 0).toLocaleString()} Tokens`}>
                <div className="analytics-trend-stack" style={{ height: `${Math.max(stat?.totalTokens ? 2 : 0, ((stat?.totalTokens ?? 0) / usageView.maxDayTokens) * 100)}%` }}>
                  {usageView.models.map((model, modelIndex) => {
                    const tokens = stat?.modelTokens[model.model] ?? 0;
                    if (!tokens || !stat?.totalTokens) return null;
                    return <i key={model.model} style={{ background: MODEL_COLORS[modelIndex % MODEL_COLORS.length], height: `${(tokens / stat.totalTokens) * 100}%` }} />;
                  })}
                </div>
              </div>
            ))}
          </div>
          <div className="analytics-trend-axis" aria-hidden="true">
            {usageView.trendDays.map(({ date, dateKey }, index) => (
              <span key={`${dateKey}-axis`}>
                {usageView.trendTickIndexes.includes(index) ? formatAnalyticsTrendDate(date) : ""}
              </span>
            ))}
          </div>
        </div>
        <div className="analytics-model-legend">
          {usageView.models.map((model, index) => <span key={model.model}><i style={{ background: MODEL_COLORS[index % MODEL_COLORS.length] }} />{model.model}</span>)}
        </div>
      </section>
      <section className="analytics-chart-card">
        <h3>{labels.modelUsage}</h3>
        <div className="analytics-model-usage">
          <div className="analytics-donut" aria-label={labels.modelUsage}>
            <svg viewBox="0 0 120 120" role="img">
              <circle className="analytics-donut__track" cx="60" cy="60" r="46" />
              {usageView.models.map((item, index) => {
                const length = analyticsModelPercent(item.totalTokens, stats.totalTokens) * 289.03;
                const offset = donutOffset;
                donutOffset += length;
                return <circle key={item.model} className="analytics-donut__segment" cx="60" cy="60" r="46" stroke={MODEL_COLORS[index % MODEL_COLORS.length]} strokeDasharray={`${Math.max(0, length - 1.4)} 289.03`} strokeDashoffset={-offset} />;
              })}
            </svg>
            <div><strong>{formatTokenCount(stats.totalTokens)}</strong><span>tokens</span></div>
          </div>
          <div className="analytics-model-breakdown">
            {usageView.models.map((item, index) => (
              <div key={item.model}>
                <i style={{ background: MODEL_COLORS[index % MODEL_COLORS.length] }} />
                <span><strong>{item.model}</strong><small>{formatTokenCount(item.totalTokens)} tokens</small></span>
                <b>{(analyticsModelPercent(item.totalTokens, stats.totalTokens) * 100).toFixed(1)}%</b>
              </div>
            ))}
          </div>
        </div>
      </section>
    </div>
  );
}

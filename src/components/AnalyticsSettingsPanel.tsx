import { useEffect, useMemo, useState } from "react";
import {
  requestRecordsList,
  usageStatsGet,
  type RequestRecord,
  type UsageStats,
} from "@/lib/api";
import { formatTokenCount } from "@/lib/contextUsage";

type Props = {
  mode: "requests" | "usage";
  labels: {
    loading: string;
    empty: string;
    time: string;
    model: string;
    requestMode: string;
    duration: string;
    tokens: string;
    details: string;
    sync: string;
    async: string;
    turn: string;
    estimated: string;
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
    sendAcknowledgement: string;
    firstSse: string;
    firstVisible: string;
    completed: string;
    cacheHit: string;
    notReported: string;
  };
};

const MODEL_COLORS = ["#1683f8", "#209447", "#9567ec", "#ed3434", "#e78000", "#0ba6a6", "#e85ca3", "#64748b"];

function localDateKey(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function recentDays(count: number) {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  return Array.from({ length: count }, (_, index) => {
    const date = new Date(today);
    date.setDate(today.getDate() - (count - index - 1));
    return date;
  });
}

function compactJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

export function formatRequestElapsed(
  requestedAtMs: number,
  atMs: number | null,
  missing: string,
): string {
  if (atMs == null || !Number.isFinite(atMs)) return missing;
  return `${Math.max(0, atMs - requestedAtMs).toLocaleString()} ms`;
}

export function formatAnalyticsCacheHitRate(
  rate: number | null,
  missing: string,
): string {
  if (rate == null || !Number.isFinite(rate) || rate < 0 || rate > 1) {
    return missing;
  }
  const percent = Math.round(rate * 1_000) / 10;
  return `${Number.isInteger(percent) ? percent.toFixed(0) : percent.toFixed(1)}%`;
}

export function formatAnalyticsRequestMode(
  mode: RequestRecord["requestMode"],
  labels: Pick<Props["labels"], "sync" | "async" | "turn">,
): string {
  if (mode === "sync") return labels.sync;
  if (mode === "async") return labels.async;
  return labels.turn;
}

export function AnalyticsSettingsPanel({ mode, labels }: Props) {
  const [records, setRecords] = useState<RequestRecord[]>([]);
  const [stats, setStats] = useState<UsageStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    const load = mode === "requests" ? requestRecordsList() : usageStatsGet();
    void load
      .then((result) => {
        if (!active) return;
        if (mode === "requests") setRecords(result as RequestRecord[]);
        else setStats(result as UsageStats);
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
  }, [mode]);

  const usageView = useMemo(() => {
    if (!stats) return null;
    const daysByDate = new Map(stats.days.map((item) => [item.date, item]));
    const heatmapDays = recentDays(364).map((date) => ({
      date,
      stat: daysByDate.get(localDateKey(date)),
    }));
    const trendDays = recentDays(31).map((date) => ({
      date,
      stat: daysByDate.get(localDateKey(date)),
    }));
    const maxDayTokens = Math.max(1, ...trendDays.map((item) => item.stat?.totalTokens ?? 0));
    const maxHeatTokens = Math.max(1, ...heatmapDays.map((item) => item.stat?.totalTokens ?? 0));
    const models = [...stats.models].sort((a, b) => b.totalTokens - a.totalTokens);
    return { heatmapDays, trendDays, maxDayTokens, maxHeatTokens, models };
  }, [stats]);

  if (loading) return <div className="analytics-empty">{labels.loading}</div>;
  if (error) return <div className="analytics-empty is-error">{error}</div>;

  if (mode === "requests") {
    if (!records.length) return <div className="analytics-empty">{labels.empty}</div>;
    return (
      <div className="analytics-table-wrap">
        <table className="analytics-table">
          <thead>
            <tr>
              <th>{labels.time}</th><th>{labels.model}</th><th>{labels.requestMode}</th>
              <th>{labels.duration}</th><th>{labels.tokens}</th><th>{labels.details}</th>
            </tr>
          </thead>
          <tbody>
            {records.map((record) => (
              <tr key={record.id}>
                <td>{new Date(record.requestedAtMs).toLocaleString()}</td>
                <td>{record.model}</td>
                <td>{formatAnalyticsRequestMode(record.requestMode, labels)}</td>
                <td>{record.durationMs.toLocaleString()} ms</td>
                <td>{record.estimated ? "~" : ""}{formatTokenCount(record.inputTokens + record.outputTokens)}</td>
                <td>
                  <details className="analytics-details">
                    <summary>{labels.details}</summary>
                    <dl className="analytics-details__grid">
                      <dt>{labels.sendAcknowledgement}</dt>
                      <dd>{formatRequestElapsed(record.requestedAtMs, record.acceptedAtMs, labels.notReported)}</dd>
                      <dt>{labels.firstSse}</dt>
                      <dd>{formatRequestElapsed(record.requestedAtMs, record.firstProviderEventAtMs, labels.notReported)}</dd>
                      <dt>{labels.firstVisible}</dt>
                      <dd>{formatRequestElapsed(record.requestedAtMs, record.firstVisibleTokenAtMs, labels.notReported)}</dd>
                      <dt>{labels.completed}</dt>
                      <dd>{formatRequestElapsed(record.requestedAtMs, record.completedAtMs, labels.notReported)}</dd>
                      <dt>{labels.cacheHit}</dt>
                      <dd>{formatAnalyticsCacheHitRate(record.cacheHitRate, labels.notReported)}</dd>
                    </dl>
                    <div className="analytics-details__grid">
                      <pre>{compactJson(record.request)}</pre>
                      <pre>{record.response}</pre>
                    </div>
                  </details>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    );
  }

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
        <div className="analytics-token-trend">
          {usageView.trendDays.map(({ date, stat }) => (
            <div className="analytics-trend-day" key={localDateKey(date)} title={`${date.toLocaleDateString()}: ${(stat?.totalTokens ?? 0).toLocaleString()} Tokens`}>
              <div className="analytics-trend-stack" style={{ height: `${Math.max(stat?.totalTokens ? 2 : 0, ((stat?.totalTokens ?? 0) / usageView.maxDayTokens) * 100)}%` }}>
                {usageView.models.map((model, modelIndex) => {
                  const tokens = stat?.modelTokens[model.model] ?? 0;
                  if (!tokens || !stat?.totalTokens) return null;
                  return <i key={model.model} style={{ background: MODEL_COLORS[modelIndex % MODEL_COLORS.length], height: `${(tokens / stat.totalTokens) * 100}%` }} />;
                })}
              </div>
              <span>{date.getDate() === 1 || date.getDate() % 5 === 0 ? `${date.getMonth() + 1}/${date.getDate()}` : ""}</span>
            </div>
          ))}
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
                const length = (item.totalTokens / stats.totalTokens) * 289.03;
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
                <b>{((item.totalTokens / stats.totalTokens) * 100).toFixed(1)}%</b>
              </div>
            ))}
          </div>
        </div>
      </section>
    </div>
  );
}

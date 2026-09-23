import { Alert, AlertDescription } from "@appica/ui-react/alert";
import { Card } from "@/components/ui/card";
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { Button } from "@appica/ui-react/button";
import {
  IconActivity,
  IconAlertTriangle,
  IconClock,
  IconDatabase,
  IconDownload,
  IconRefresh,
} from "@/components/icons";
import {
  defaultObservabilityApi,
  normalizeObservabilitySnapshot,
  observabilitySummary,
  type ObservabilityApi,
  type ObservabilitySnapshot,
} from "@/lib/observability";
import "@/styles/observability.css";

export interface RuntimeObservabilityLabels {
  title: string;
  description: string;
  refresh: string;
  refreshing: string;
  export: string;
  exporting: string;
  loading: string;
  unavailable: string;
  events: string;
  traces: string;
  ttftP50: string;
  ttftP95: string;
  resources: string;
  crashes: string;
  dropped: string;
  startup: string;
  latestResource: string;
  latestTrace: string;
  latestCrash: string;
  noData: string;
  noCrashes: string;
  noResources: string;
  noStartup: string;
  metric: string;
  count: string;
  average: string;
  range: string;
  cpu: string;
  processMemory: string;
  privateMemory: string;
  processCount: string;
  frontendMemory: string;
  domNodes: string;
  eventLoopLag: string;
  longTasks: string;
  phase: string;
  elapsed: string;
  status: string;
  time: string;
}

export interface RuntimeObservabilityPanelProps {
  labels: RuntimeObservabilityLabels;
  api?: ObservabilityApi;
  /** 导出 JSON 的宿主处理器；未提供时下载一个本地文件。 */
  onExport?: (json: string) => void | Promise<void>;
}

export interface ObservabilityPanelViewProps {
  labels: RuntimeObservabilityLabels;
  snapshot: ObservabilitySnapshot | null;
  loading?: boolean;
  refreshing?: boolean;
  exporting?: boolean;
  error?: string | null;
  onRefresh?: () => void;
  onExport?: () => void;
}

export function RuntimeObservabilityPanel({
  labels,
  api = defaultObservabilityApi,
  onExport,
}: RuntimeObservabilityPanelProps) {
  const [snapshot, setSnapshot] = useState<ObservabilitySnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadSnapshot = useCallback(async (initial = false) => {
    if (initial) setLoading(true);
    else setRefreshing(true);
    try {
      const value = normalizeObservabilitySnapshot(await api.snapshot());
      setSnapshot(value);
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (initial) setLoading(false);
      else setRefreshing(false);
    }
  }, [api]);

  useEffect(() => {
    let disposed = false;
    let refreshTimer: number | null = null;
    let unsubscribe: (() => void) | null = null;
    void loadSnapshot(true);
    void api.subscribe(() => {
      if (disposed || refreshTimer !== null) return;
      refreshTimer = window.setTimeout(() => {
        refreshTimer = null;
        if (!disposed) void loadSnapshot();
      }, 250);
    }).then((cleanup) => {
      if (disposed) cleanup();
      else unsubscribe = cleanup;
    }).catch(() => {
      // 浏览器开发服务器没有 Tauri 事件总线；手动刷新仍可用。
    });
    return () => {
      disposed = true;
      if (refreshTimer !== null) window.clearTimeout(refreshTimer);
      unsubscribe?.();
    };
  }, [api, loadSnapshot]);

  const exportSnapshot = useCallback(async () => {
    setExporting(true);
    try {
      const json = await api.exportRedacted();
      if (onExport) {
        await onExport(json);
      } else {
        downloadObservabilityJson(json);
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setExporting(false);
    }
  }, [api, onExport]);

  return (
    <ObservabilityPanelView
      labels={labels}
      snapshot={snapshot}
      loading={loading}
      refreshing={refreshing}
      exporting={exporting}
      error={error}
      onRefresh={() => void loadSnapshot()}
      onExport={() => void exportSnapshot()}
    />
  );
}

/**
 * 纯展示视图：不读取宿主能力，便于 Web、移动远程壳和状态矩阵共用。
 * 所有按钮动作都由调用方注入，避免组件内部建立第二套运行时事实源。
 */
export function ObservabilityPanelView({
  labels,
  snapshot,
  loading = false,
  refreshing = false,
  exporting = false,
  error = null,
  onRefresh,
  onExport,
}: ObservabilityPanelViewProps) {
  const summary = useMemo(
    () => (snapshot ? observabilitySummary(snapshot) : null),
    [snapshot],
  );
  const ttft = snapshot?.histograms.find((item) => item.name === "runtime.ttft_ms");

  if (loading) {
    return <div className="runtime-observability__state">{labels.loading}</div>;
  }

  return (
    <section className="runtime-observability" data-testid="runtime-observability">
      <div className="runtime-observability__heading">
        <div>
          <h2 className="settings-page__h2">{labels.title}</h2>
          <p className="settings-page__lead">{labels.description}</p>
        </div>
        <div className="runtime-observability__actions">
          <Button
            type="button"
            variant="ghost"
            size="md"
            onClick={onRefresh}
            disabled={!onRefresh || refreshing}
            aria-label={refreshing ? labels.refreshing : labels.refresh}
            title={refreshing ? labels.refreshing : labels.refresh}
          >
            <IconRefresh size={15} />
            <span>{refreshing ? labels.refreshing : labels.refresh}</span>
          </Button>
          <Button
            type="button"
            variant="outline"
            size="md"
            onClick={onExport}
            disabled={!onExport || exporting}
            aria-label={exporting ? labels.exporting : labels.export}
            title={exporting ? labels.exporting : labels.export}
          >
            <IconDownload size={15} />
            <span>{exporting ? labels.exporting : labels.export}</span>
          </Button>
        </div>
      </div>

      {error ? (
        <Alert variant="error">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      {snapshot && summary ? (
        <>
          <div className="runtime-observability__kpis">
            <SummaryCard icon={<IconActivity size={16} />} label={labels.events} value={String(snapshot.events.length)} />
            <SummaryCard icon={<IconDatabase size={16} />} label={labels.traces} value={String(snapshot.traces.length)} />
            <SummaryCard icon={<IconClock size={16} />} label={labels.ttftP50} value={formatDuration(summary.ttftP50Ms, labels.noData)} />
            <SummaryCard icon={<IconClock size={16} />} label={labels.ttftP95} value={formatDuration(summary.ttftP95Ms, labels.noData)} />
            <SummaryCard icon={<IconDatabase size={16} />} label={labels.resources} value={String(snapshot.resourceSamples.length)} />
            <SummaryCard icon={<IconAlertTriangle size={16} />} label={labels.crashes} value={String(snapshot.crashes.length)} />
          </div>

          <div className="runtime-observability__grid">
            <Card render={<section />} className="runtime-observability__section">
              <div className="runtime-observability__section-heading">
                <h3>{labels.startup}</h3>
                <span>{snapshot.startupPhases.length}</span>
              </div>
              {summary.latestStartup ? (
                <div className="runtime-observability__detail-list">
                  {snapshot.startupPhases.slice(-8).map((phase) => (
                    <div className="runtime-observability__detail-row" key={`${phase.phase}-${phase.occurredAtMs}`}>
                      <span>{phase.phase}</span>
                      <strong>{formatDuration(phase.elapsedMs, labels.noData)}</strong>
                    </div>
                  ))}
                </div>
              ) : <div className="runtime-observability__empty">{labels.noStartup}</div>}
            </Card>

            <Card render={<section />} className="runtime-observability__section">
              <div className="runtime-observability__section-heading">
                <h3>{labels.latestResource}</h3>
                <span>{summary.latestResource ? formatTime(summary.latestResource.occurredAtMs) : labels.noData}</span>
              </div>
              {summary.latestResource ? (
                <div className="runtime-observability__detail-list">
                  <DetailRow label={labels.cpu} value={formatPercent(summary.latestResource.cpuPercent, labels.noData)} />
                  <DetailRow label={labels.processMemory} value={formatBytes(summary.latestResource.residentBytes, labels.noData)} />
                  <DetailRow label={labels.privateMemory} value={formatBytes(summary.latestResource.privateBytes, labels.noData)} />
                  <DetailRow label={labels.processCount} value={formatCount(summary.latestResource.processCount, labels.noData)} />
                  <DetailRow label={labels.frontendMemory} value={formatBytes(summary.latestResource.frontendHeapUsedBytes, labels.noData)} />
                  <DetailRow label={labels.domNodes} value={formatCount(summary.latestResource.domNodes, labels.noData)} />
                  <DetailRow label={labels.eventLoopLag} value={formatDuration(summary.latestResource.eventLoopLagMs, labels.noData)} />
                  <DetailRow label={labels.longTasks} value={formatCount(summary.latestResource.longTaskCount, labels.noData)} />
                </div>
              ) : <div className="runtime-observability__empty">{labels.noResources}</div>}
            </Card>
          </div>

          <Card render={<section />} className="runtime-observability__section">
            <div className="runtime-observability__section-heading">
              <h3>{labels.latestTrace}</h3>
              <span>{ttft ? `${labels.count}: ${ttft.count}` : labels.noData}</span>
            </div>
            {snapshot.traces.length ? (
              <div className="runtime-observability__table-wrap">
                <table className="runtime-observability__table">
                  <thead><tr><th>{labels.metric}</th><th>{labels.status}</th><th>{labels.ttftP50}</th><th>{labels.time}</th></tr></thead>
                  <tbody>
                    {snapshot.traces.slice(-8).reverse().map((trace) => (
                      <tr key={`${trace.traceId}-${trace.spanId}`}>
                        <td>{trace.name}</td>
                        <td>{trace.status}</td>
                        <td>{formatDuration(trace.ttftMs, labels.noData)}</td>
                        <td>{formatDuration(trace.durationMs, labels.noData)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : <div className="runtime-observability__empty">{labels.noData}</div>}
          </Card>

          <div className="runtime-observability__footer">
            <span>{labels.dropped}: {snapshot.droppedRealtimeEvents}</span>
            <span>{labels.latestCrash}: {snapshot.crashes.length ? formatTime(snapshot.crashes.at(-1)?.occurredAtMs ?? 0) : labels.noCrashes}</span>
          </div>
        </>
      ) : (
        <div className="runtime-observability__state">{labels.unavailable}</div>
      )}
    </section>
  );
}

function SummaryCard({ icon, label, value }: { icon: ReactNode; label: string; value: string }) {
  return <div className="runtime-observability__kpi"><span className="runtime-observability__kpi-icon">{icon}</span><span>{label}</span><strong>{value}</strong></div>;
}

function DetailRow({ label, value }: { label: string; value: string }) {
  return <div className="runtime-observability__detail-row"><span>{label}</span><strong>{value}</strong></div>;
}

function formatDuration(value: number | null | undefined, empty: string): string {
  if (value == null || !Number.isFinite(value)) return empty;
  return `${Math.round(value * 10) / 10} ms`;
}

function formatBytes(value: number | null | undefined, empty: string): string {
  if (value == null || !Number.isFinite(value)) return empty;
  if (value < 1024) return `${Math.round(value)} B`;
  const units = ["KiB", "MiB", "GiB"];
  let scaled = value / 1024;
  let unit = units[0];
  for (let index = 1; index < units.length && scaled >= 1024; index += 1) {
    scaled /= 1024;
    unit = units[index];
  }
  return `${scaled.toFixed(scaled >= 10 ? 0 : 1)} ${unit}`;
}

function formatPercent(value: number | null | undefined, empty: string): string {
  if (value == null || !Number.isFinite(value)) return empty;
  return `${Math.round(value * 10) / 10}%`;
}

function formatCount(value: number | null | undefined, empty: string): string {
  if (value == null || !Number.isFinite(value)) return empty;
  return Math.round(value).toLocaleString();
}

function formatTime(value: number): string {
  if (!value || !Number.isFinite(value)) return "-";
  return new Date(value).toLocaleTimeString();
}

function downloadObservabilityJson(json: string): void {
  const blob = new Blob([json], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `keencode-observability-${new Date().toISOString().replace(/[:.]/g, "-")}.json`;
  link.click();
  URL.revokeObjectURL(url);
}

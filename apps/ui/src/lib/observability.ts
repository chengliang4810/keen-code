import { listen } from "@tauri-apps/api/event";
import { invoke, isTauri } from "./tauri";

export const OBSERVABILITY_SCHEMA = 1;

export interface RetentionPolicy {
  metricPoints: number;
  traceSamples: number;
  resourceSamples: number;
  startupPhases: number;
  crashRecords: number;
  eventRecords: number;
  realtimeSubscriberCapacity: number;
  maxExportBytes: number;
}

export interface MetricPoint {
  name: string;
  value: number;
  unit: string;
  occurredAtMs: number;
  tags: Record<string, string>;
}

export interface HistogramSnapshot {
  name: string;
  boundaries: number[];
  bucketCounts: number[];
  count: number;
  sum: number;
  min: number | null;
  max: number | null;
}

export interface TraceSample {
  traceId: string;
  spanId: string;
  parentSpanId: string | null;
  name: string;
  startedAtMs: number;
  durationMs: number | null;
  ttftMs: number | null;
  status: string;
  attributes: Record<string, string>;
}

export interface ResourceSample {
  occurredAtMs: number;
  processId: number;
  cpuPercent: number | null;
  residentBytes: number | null;
  privateBytes: number | null;
  virtualBytes: number | null;
  processCount: number | null;
  frontendHeapUsedBytes: number | null;
  frontendHeapLimitBytes: number | null;
  domNodes: number | null;
  eventLoopLagMs: number | null;
  longTaskCount: number | null;
}

export interface StartupPhase {
  phase: string;
  occurredAtMs: number;
  elapsedMs: number;
}

export interface CrashRecord {
  occurredAtMs: number;
  kind: string;
  message: string;
  backtrace: string | null;
}

export interface ObservabilityEvent {
  sequence: number;
  occurredAtMs: number;
  kind: string;
  payload: unknown;
}

export interface ObservabilitySnapshot {
  schema: number;
  capturedAtMs: number;
  retention: RetentionPolicy;
  counters: Record<string, number>;
  gauges: Record<string, number>;
  metricPoints: MetricPoint[];
  histograms: HistogramSnapshot[];
  traces: TraceSample[];
  resourceSamples: ResourceSample[];
  startupPhases: StartupPhase[];
  crashes: CrashRecord[];
  events: ObservabilityEvent[];
  droppedRealtimeEvents: number;
}

export interface FrontendResourceSample {
  occurredAtMs: number;
  processId: number;
  frontendHeapUsedBytes: number | null;
  frontendHeapLimitBytes: number | null;
  domNodes: number | null;
  eventLoopLagMs: number | null;
  longTaskCount: number | null;
}

export type FrontendResourceSamplingMode = "panel" | "active" | "idle";

export interface FrontendResourceSampler {
  setMode: (mode: FrontendResourceSamplingMode) => void;
  stop: () => void;
}

export interface ObservabilityApi {
  snapshot: () => Promise<ObservabilitySnapshot>;
  exportRedacted: () => Promise<string>;
  recordMetric: (
    name: string,
    value: number,
    unit: string,
    tags?: Record<string, string>,
  ) => Promise<void>;
  recordResourceSample: (sample: ResourceSample) => Promise<void>;
  recordTrace: (trace: TraceSample) => Promise<void>;
  subscribe: (handler: (event: ObservabilityEvent) => void) => Promise<() => void>;
}

/** 只读快照；后端必须在命令边界再次校验并脱敏。 */
export function observabilitySnapshot(): Promise<ObservabilitySnapshot> {
  return invoke<ObservabilitySnapshot>("diagnostics_snapshot");
}

/** 返回已经由后端结构化脱敏的 JSON；前端不拼接原始日志。 */
export function observabilityExportRedacted(): Promise<string> {
  return invoke<string>("diagnostics_export");
}

export function observabilityRecordMetric(
  name: string,
  value: number,
  unit: string,
  tags: Record<string, string> = {},
): Promise<void> {
  return invoke<void>("diagnostics_metric_record", { name, value, unit, tags });
}

export function observabilityRecordResourceSample(sample: ResourceSample): Promise<void> {
  return invoke<void>("diagnostics_resource_record", { sample });
}

export function observabilityRecordTrace(trace: TraceSample): Promise<void> {
  return invoke<void>("diagnostics_trace_record", { trace });
}

/**
 * 订阅 Tauri 实时观测事件。浏览器开发服务器没有原生总线，返回空取消函数，
 * 面板仍可用手动刷新查看最近快照。
 */
export async function observabilitySubscribe(
  handler: (event: ObservabilityEvent) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const unlisten = await listen<ObservabilityEvent>("keencode://observability", (event) => {
    handler(event.payload);
  });
  return unlisten;
}

export const defaultObservabilityApi: ObservabilityApi = {
  snapshot: observabilitySnapshot,
  exportRedacted: observabilityExportRedacted,
  recordMetric: observabilityRecordMetric,
  recordResourceSample: observabilityRecordResourceSample,
  recordTrace: observabilityRecordTrace,
  subscribe: observabilitySubscribe,
};

/** 浏览器展示夹具：不触碰 Tauri IPC，允许 showcase/Story fixture 注入受控快照。 */
export function createBrowserObservabilityApi(
  fixture: ObservabilitySnapshot = emptyObservabilitySnapshot(),
): ObservabilityApi {
  const snapshot = normalizeObservabilitySnapshot(fixture);
  return {
    snapshot: async () => snapshot,
    exportRedacted: async () => JSON.stringify(snapshot),
    recordMetric: async () => {},
    recordResourceSample: async () => {},
    recordTrace: async () => {},
    subscribe: async () => () => {},
  };
}

export const browserObservabilityApi = createBrowserObservabilityApi();

export function emptyObservabilitySnapshot(): ObservabilitySnapshot {
  return {
    schema: OBSERVABILITY_SCHEMA,
    capturedAtMs: 0,
    retention: {
      metricPoints: 0,
      traceSamples: 0,
      resourceSamples: 0,
      startupPhases: 0,
      crashRecords: 0,
      eventRecords: 0,
      realtimeSubscriberCapacity: 0,
      maxExportBytes: 0,
    },
    counters: {},
    gauges: {},
    metricPoints: [],
    histograms: [],
    traces: [],
    resourceSamples: [],
    startupPhases: [],
    crashes: [],
    events: [],
    droppedRealtimeEvents: 0,
  };
}

/** 产生端只用 performance.now() 计算时长；startedAtMs 仅用于排序。 */
export function startFrontendTrace(
  name: string,
  attributes: Record<string, string> = {},
  now: () => number = () => performance.now(),
  epoch: () => number = () => Date.now(),
): {
  markTtft: () => number;
  finish: (status?: string) => Promise<TraceSample>;
} {
  const started = now();
  const startedAtMs = epoch();
  let ttftMs: number | null = null;
  return {
    markTtft: () => {
      if (ttftMs === null) ttftMs = Math.max(0, Math.round(now() - started));
      return ttftMs;
    },
    finish: async (status = "ok") => {
      const durationMs = Math.max(0, Math.round(now() - started));
      const trace: TraceSample = {
        traceId: `frontend-${startedAtMs}-${Math.round(started)}`,
        spanId: `span-${Math.round(started)}`,
        parentSpanId: null,
        name,
        startedAtMs,
        durationMs,
        ttftMs,
        status,
        attributes,
      };
      await observabilityRecordTrace(trace);
      return trace;
    },
  };
}

/** 采集可选的 WebView 资源指标；不存在的浏览器 API 保持 null。 */
export function collectFrontendResourceSample(
  now: () => number = () => Date.now(),
  root: Document | null = typeof document === "undefined" ? null : document,
): FrontendResourceSample {
  const memory = (performance as Performance & {
    memory?: { usedJSHeapSize: number; jsHeapSizeLimit: number };
  }).memory;
  const domNodes = root ? root.getElementsByTagName("*").length : null;
  return {
    occurredAtMs: now(),
    processId: 0,
    frontendHeapUsedBytes: memory?.usedJSHeapSize ?? null,
    frontendHeapLimitBytes: memory?.jsHeapSizeLimit ?? null,
    domNodes,
    eventLoopLagMs: null,
    longTaskCount: null,
  };
}

const FRONTEND_RESOURCE_INTERVALS: Record<Exclude<FrontendResourceSamplingMode, "idle">, number> = {
  panel: 1_000,
  active: 5_000,
};

function createResourceSampler(
  onSample: (sample: FrontendResourceSample) => void,
  intervalForMode: (mode: FrontendResourceSamplingMode) => number | null,
  initialMode: FrontendResourceSamplingMode,
): FrontendResourceSampler {
  let stopped = false;
  let mode: FrontendResourceSamplingMode = "idle";
  let timer: ReturnType<typeof setInterval> | null = null;
  let expectedAt: number | null = null;
  let longTaskCount = 0;
  let longTaskObserver: PerformanceObserver | null = null;

  const startLongTaskObserver = () => {
    if (
      longTaskObserver ||
      typeof PerformanceObserver === "undefined" ||
      !(PerformanceObserver.supportedEntryTypes?.includes("longtask") ?? false)
    ) return;
    try {
      longTaskObserver = new PerformanceObserver((list) => {
        longTaskCount += list.getEntries().length;
      });
      longTaskObserver.observe({ entryTypes: ["longtask"] });
    } catch {
      longTaskObserver?.disconnect();
      longTaskObserver = null;
    }
  };

  const stopLongTaskObserver = () => {
    longTaskObserver?.disconnect();
    longTaskObserver = null;
  };

  const clearTimer = () => {
    if (timer === null) return;
    clearInterval(timer);
    timer = null;
  };

  const sample = (interval: number) => {
    if (stopped) return;
    const now = performance.now();
    const lag = expectedAt === null ? 0 : Math.max(0, now - expectedAt);
    expectedAt = now + interval;
    const value = collectFrontendResourceSample();
    onSample({
      ...value,
      eventLoopLagMs: Math.round(lag * 10) / 10,
      longTaskCount: longTaskObserver ? longTaskCount : null,
    });
  };

  const setMode = (nextMode: FrontendResourceSamplingMode) => {
    if (stopped || nextMode === mode) return;
    clearTimer();
    mode = nextMode;
    expectedAt = null;
    const interval = intervalForMode(nextMode);
    if (interval === null) {
      stopLongTaskObserver();
      return;
    }
    startLongTaskObserver();
    expectedAt = performance.now() + interval;
    sample(interval);
    timer = setInterval(() => sample(interval), interval);
  };

  const stop = () => {
    if (stopped) return;
    stopped = true;
    clearTimer();
    stopLongTaskObserver();
    expectedAt = null;
    mode = "idle";
  };

  const sampler = { setMode, stop } satisfies FrontendResourceSampler;
  setMode(initialMode);
  return sampler;
}

/** 按界面可见状态切换前端资源采样频率；`idle` 会清除定时器。 */
export function createFrontendResourceSampler(
  onSample: (sample: FrontendResourceSample) => void,
  initialMode: FrontendResourceSamplingMode = "active",
): FrontendResourceSampler {
  return createResourceSampler(
    onSample,
    (mode) => (mode === "idle" ? null : FRONTEND_RESOURCE_INTERVALS[mode]),
    initialMode,
  );
}

/** 兼容固定周期调用方；新界面应使用 `createFrontendResourceSampler`。 */
export function startFrontendResourceSampler(
  onSample: (sample: FrontendResourceSample) => void,
  intervalMs = 10_000,
): () => void {
  const interval = Math.max(1_000, Math.floor(intervalMs));
  return createResourceSampler(onSample, (mode) => (mode === "idle" ? null : interval), "active").stop;
}

/** 后端桶是独立计数，按桶累计值估算最近的 p50/p95，空样本返回 null。 */
export function histogramPercentile(
  histogram: HistogramSnapshot | undefined,
  percentile: number,
): number | null {
  if (!histogram || histogram.count <= 0 || !histogram.bucketCounts.length) return null;
  const target = Math.max(1, Math.ceil(histogram.count * Math.min(1, Math.max(0, percentile))));
  let cumulative = 0;
  for (let index = 0; index < histogram.bucketCounts.length; index += 1) {
    cumulative += histogram.bucketCounts[index] ?? 0;
    if (cumulative >= target) return histogram.boundaries[index] ?? histogram.max ?? null;
  }
  return histogram.max;
}

export function observabilitySummary(snapshot: ObservabilitySnapshot): {
  ttftP50Ms: number | null;
  ttftP95Ms: number | null;
  latestStartup: StartupPhase | null;
  latestResource: ResourceSample | null;
} {
  const ttft = snapshot.histograms.find((item) => item.name === "runtime.ttft_ms");
  return {
    ttftP50Ms: histogramPercentile(ttft, 0.5),
    ttftP95Ms: histogramPercentile(ttft, 0.95),
    latestStartup: snapshot.startupPhases.at(-1) ?? null,
    latestResource: snapshot.resourceSamples.at(-1) ?? null,
  };
}

export function normalizeObservabilitySnapshot(value: unknown): ObservabilitySnapshot {
  if (typeof value !== "object" || value === null) throw new Error("观测快照格式无效");
  const input = value as Partial<ObservabilitySnapshot>;
  if (input.schema !== OBSERVABILITY_SCHEMA) throw new Error("观测快照版本不兼容");
  return {
    schema: OBSERVABILITY_SCHEMA,
    capturedAtMs: finiteNumber(input.capturedAtMs),
    retention: input.retention ?? {
      metricPoints: 0,
      traceSamples: 0,
      resourceSamples: 0,
      startupPhases: 0,
      crashRecords: 0,
      eventRecords: 0,
      realtimeSubscriberCapacity: 0,
      maxExportBytes: 0,
    },
    counters: input.counters ?? {},
    gauges: input.gauges ?? {},
    metricPoints: Array.isArray(input.metricPoints) ? input.metricPoints : [],
    histograms: Array.isArray(input.histograms) ? input.histograms : [],
    traces: Array.isArray(input.traces) ? input.traces : [],
    resourceSamples: Array.isArray(input.resourceSamples) ? input.resourceSamples : [],
    startupPhases: Array.isArray(input.startupPhases) ? input.startupPhases : [],
    crashes: Array.isArray(input.crashes) ? input.crashes : [],
    events: Array.isArray(input.events) ? input.events : [],
    droppedRealtimeEvents: finiteNumber(input.droppedRealtimeEvents),
  };
}

function finiteNumber(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

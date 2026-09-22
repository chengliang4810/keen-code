import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  collectFrontendResourceSample,
  createBrowserObservabilityApi,
  histogramPercentile,
  normalizeObservabilitySnapshot,
  observabilityExportRedacted,
  observabilityRecordMetric,
  observabilityRecordResourceSample,
  observabilitySnapshot,
  observabilitySummary,
  observabilitySubscribe,
  createFrontendResourceSampler,
  startFrontendTrace,
  type HistogramSnapshot,
} from "./observability";
import { invoke } from "./tauri";

vi.mock("./tauri", () => ({
  invoke: vi.fn().mockResolvedValue(undefined),
  isTauri: vi.fn().mockReturnValue(false),
}));

const ttftHistogram: HistogramSnapshot = {
  name: "runtime.ttft_ms",
  boundaries: [10, 100, 1_000],
  bucketCounts: [2, 2, 1, 0],
  count: 5,
  sum: 1_200,
  min: 5,
  max: 500,
};

describe("observability frontend contract", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockClear();
  });

  it("按独立桶计数估算 TTFT p50/p95，不把 bucketCounts 当累计值", () => {
    expect(histogramPercentile(ttftHistogram, 0.5)).toBe(100);
    expect(histogramPercentile(ttftHistogram, 0.95)).toBe(1000);
    expect(histogramPercentile(undefined, 0.5)).toBeNull();
  });

  it("前端 Trace 只发送产生端单调时长和排序时间戳", async () => {
    let monotonic = 100;
    const trace = startFrontendTrace(
      "frontend.render",
      { surface: "chat" },
      () => monotonic,
      () => 1_700_000_000_000,
    );
    monotonic = 112;
    expect(trace.markTtft()).toBe(12);
    monotonic = 150;
    const finished = await trace.finish();
    expect(finished.durationMs).toBe(50);
    expect(finished.ttftMs).toBe(12);
    expect(finished.startedAtMs).toBe(1_700_000_000_000);
    expect(invoke).toHaveBeenCalledWith("diagnostics_trace_record", { trace: finished });
  });

  it("观测命令包装使用稳定的 Tauri 命令和参数结构", async () => {
    const sample = collectFrontendResourceSample(() => 42, null);
    await observabilitySnapshot();
    await observabilityExportRedacted();
    await observabilityRecordMetric("frontend.test", 1, "count", { surface: "test" });
    await observabilityRecordResourceSample({
      ...sample,
      cpuPercent: null,
      residentBytes: null,
      privateBytes: null,
      virtualBytes: null,
      processCount: null,
    });

    expect(invoke).toHaveBeenNthCalledWith(1, "diagnostics_snapshot");
    expect(invoke).toHaveBeenNthCalledWith(2, "diagnostics_export");
    expect(invoke).toHaveBeenNthCalledWith(3, "diagnostics_metric_record", {
      name: "frontend.test",
      value: 1,
      unit: "count",
      tags: { surface: "test" },
    });
    expect(invoke).toHaveBeenNthCalledWith(4, "diagnostics_resource_record", {
      sample: {
        ...sample,
        cpuPercent: null,
        residentBytes: null,
        privateBytes: null,
        virtualBytes: null,
        processCount: null,
      },
    });
  });

  it("资源采样在缺少浏览器专属 API 时保持 null", () => {
    const sample = collectFrontendResourceSample(() => 42, null);
    expect(sample).toMatchObject({
      occurredAtMs: 42,
      processId: 0,
      frontendHeapUsedBytes: null,
      frontendHeapLimitBytes: null,
      domNodes: null,
    });
  });

  it("按 panel、active、idle 模式切换采样定时器", () => {
    vi.useFakeTimers();
    try {
      const onSample = vi.fn();
      const sampler = createFrontendResourceSampler(onSample, "panel");
      expect(onSample).toHaveBeenCalledTimes(1);

      vi.advanceTimersByTime(999);
      expect(onSample).toHaveBeenCalledTimes(1);
      vi.advanceTimersByTime(1);
      expect(onSample).toHaveBeenCalledTimes(2);

      sampler.setMode("active");
      expect(onSample).toHaveBeenCalledTimes(3);
      vi.advanceTimersByTime(4_999);
      expect(onSample).toHaveBeenCalledTimes(3);
      vi.advanceTimersByTime(1);
      expect(onSample).toHaveBeenCalledTimes(4);

      sampler.setMode("idle");
      vi.advanceTimersByTime(10_000);
      expect(onSample).toHaveBeenCalledTimes(4);
      sampler.stop();
      sampler.setMode("panel");
      vi.advanceTimersByTime(2_000);
      expect(onSample).toHaveBeenCalledTimes(4);
    } finally {
      vi.useRealTimers();
    }
  });

  it("资源采样仅在活跃模式观察 Long Task，并在 idle 时释放", () => {
    vi.useFakeTimers();
    const disconnect = vi.fn();
    let callback: PerformanceObserverCallback | null = null;
    class MockPerformanceObserver {
      static supportedEntryTypes = ["longtask"];
      constructor(next: PerformanceObserverCallback) { callback = next; }
      observe() {}
      disconnect() { disconnect(); }
      takeRecords(): PerformanceEntryList { return []; }
    }
    vi.stubGlobal("PerformanceObserver", MockPerformanceObserver);
    try {
      const samples: Array<{ longTaskCount: number | null }> = [];
      const sampler = createFrontendResourceSampler((sample) => samples.push(sample), "active");
      const list = { getEntries: () => [{ duration: 60 }, { duration: 75 }] };
      expect(callback).not.toBeNull();
      (callback as unknown as PerformanceObserverCallback)(
        list as PerformanceObserverEntryList,
        {} as PerformanceObserver,
      );
      vi.advanceTimersByTime(5_000);
      expect(samples.at(-1)?.longTaskCount).toBe(2);

      sampler.setMode("idle");
      expect(disconnect).toHaveBeenCalledTimes(1);
      sampler.stop();
    } finally {
      vi.unstubAllGlobals();
      vi.useRealTimers();
    }
  });

  it("浏览器适配器没有 Tauri 事件总线时返回可调用的空取消函数", async () => {
    const cleanup = await observabilitySubscribe(() => {});
    expect(cleanup).toEqual(expect.any(Function));
    expect(() => cleanup()).not.toThrow();
  });

  it("受控浏览器观测 adapter 不调用 IPC，并能导出 fixture", async () => {
    const api = createBrowserObservabilityApi({
      ...emptySnapshot(),
      counters: { fixture: 1 },
    });
    await expect(api.snapshot()).resolves.toMatchObject({ counters: { fixture: 1 } });
    await expect(api.exportRedacted()).resolves.toContain('"fixture":1');
    await api.recordMetric("fixture", 1, "count");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("快照版本和数组边界经过前端命令边界校验", () => {
    const snapshot = normalizeObservabilitySnapshot({
      schema: 1,
      capturedAtMs: 10,
      droppedRealtimeEvents: 1,
      metricPoints: [],
      histograms: [ttftHistogram],
      traces: [],
      resourceSamples: [],
      startupPhases: [{ phase: "runtime_ready", elapsedMs: 20, occurredAtMs: 10 }],
      crashes: [],
      events: [],
    });
    expect(observabilitySummary(snapshot)).toMatchObject({
      ttftP50Ms: 100,
      ttftP95Ms: 1000,
      latestStartup: { phase: "runtime_ready" },
      latestResource: null,
    });
    expect(() => normalizeObservabilitySnapshot({ schema: 2 })).toThrow(/版本/);
  });
});

function emptySnapshot() {
  return {
    schema: 1,
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

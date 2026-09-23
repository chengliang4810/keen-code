import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { ObservabilityPanelView } from "./ObservabilityPanel";
import type { RuntimeObservabilityLabels } from "./ObservabilityPanel";
import type { ObservabilitySnapshot } from "@/lib/observability";

const observabilityLabels: RuntimeObservabilityLabels = {
  title: "运行观测",
  description: "展示脱敏后的本地运行时指标与会话追踪。",
  refresh: "刷新",
  refreshing: "刷新中",
  export: "导出",
  exporting: "导出中",
  loading: "正在读取观测数据…",
  unavailable: "观测数据暂不可用",
  events: "事件",
  traces: "追踪",
  ttftP50: "TTFT P50",
  ttftP95: "TTFT P95",
  resources: "资源采样",
  crashes: "崩溃记录",
  dropped: "丢弃事件",
  startup: "启动阶段",
  latestResource: "最近资源",
  latestTrace: "最近追踪",
  latestCrash: "最近崩溃",
  noData: "无数据",
  noCrashes: "无崩溃",
  noResources: "无资源采样",
  noStartup: "无启动阶段",
  metric: "指标",
  count: "数量",
  average: "平均值",
  range: "范围",
  cpu: "CPU",
  processMemory: "进程内存",
  privateMemory: "专用内存",
  processCount: "跟踪进程数",
  frontendMemory: "前端内存",
  domNodes: "DOM 节点",
  eventLoopLag: "事件循环延迟",
  longTasks: "长任务",
  phase: "阶段",
  elapsed: "耗时",
  status: "状态",
  time: "时间",
};

const emptySnapshot: ObservabilitySnapshot = {
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

const dataSnapshot: ObservabilitySnapshot = {
  ...emptySnapshot,
  capturedAtMs: 1_700_000_000_000,
  histograms: [{
    name: "runtime.ttft_ms",
    boundaries: [20, 80, 160],
    bucketCounts: [2, 3, 1, 0],
    count: 6,
    sum: 420,
    min: 12,
    max: 160,
  }],
  traces: [{
    traceId: "test-trace",
    spanId: "test-span",
    parentSpanId: null,
    name: "runtime.turn",
    startedAtMs: 1_700_000_000_000,
    durationMs: 480,
    ttftMs: 80,
    status: "ok",
    attributes: { surface: "test" },
  }],
  resourceSamples: [{
    occurredAtMs: 1_700_000_000_100,
    processId: 0,
    cpuPercent: 12.5,
    residentBytes: 64 * 1024 * 1024,
    privateBytes: 48 * 1024 * 1024,
    virtualBytes: 96 * 1024 * 1024,
    processCount: 2,
    frontendHeapUsedBytes: 8 * 1024 * 1024,
    frontendHeapLimitBytes: 64 * 1024 * 1024,
    domNodes: 120,
    eventLoopLagMs: 2,
    longTaskCount: 0,
  }],
  startupPhases: [{
    phase: "runtime_ready",
    occurredAtMs: 1_700_000_000_200,
    elapsedMs: 320,
  }],
  events: [{
    sequence: 1,
    occurredAtMs: 1_700_000_000_300,
    kind: "turn.completed",
    payload: { surface: "test" },
  }],
};

describe("ObservabilityPanelView", () => {
  it("覆盖 loading、error、empty 和 data 展示态", () => {
    const loading = renderToStaticMarkup(
      <ObservabilityPanelView
        labels={observabilityLabels}
        snapshot={null}
        loading
      />,
    );
    expect(loading).toContain("正在读取观测数据");
    expect(loading).not.toContain('data-testid="runtime-observability"');

    const error = renderToStaticMarkup(
      <ObservabilityPanelView
        labels={observabilityLabels}
        snapshot={null}
        error="观测服务错误"
        onRefresh={vi.fn()}
        onExport={vi.fn()}
      />,
    );
    expect(error).toContain("观测服务错误");
    expect(error).toContain('aria-label="刷新"');
    expect(error).toContain('aria-label="导出"');

    const empty = renderToStaticMarkup(
      <ObservabilityPanelView
        labels={observabilityLabels}
        snapshot={emptySnapshot}
        onRefresh={vi.fn()}
        onExport={vi.fn()}
      />,
    );
    expect(empty).toContain("无启动阶段");
    expect(empty).toContain("无资源采样");
    expect(empty).toContain("无数据");

    const data = renderToStaticMarkup(
      <ObservabilityPanelView
        labels={observabilityLabels}
        snapshot={dataSnapshot}
        onRefresh={vi.fn()}
        onExport={vi.fn()}
      />,
    );
    expect(data).toContain("runtime.turn");
    expect(data).toContain("64 MiB");
    expect(data).toContain("48 MiB");
    expect(data).toContain("12.5%");
    expect(data).toContain("80 ms");
    expect(data).not.toContain('aria-disabled="true"');
  });

  it("刷新和导出进行中时保留可见状态", () => {
    const html = renderToStaticMarkup(
      <ObservabilityPanelView
        labels={observabilityLabels}
        snapshot={emptySnapshot}
        refreshing
        exporting
        onRefresh={vi.fn()}
        onExport={vi.fn()}
      />,
    );
    expect(html).toContain("刷新中");
    expect(html).toContain("导出中");
    expect(html).toContain("disabled");
  });
});

import { Card } from "@/components/ui/card";
import {
  ObservabilityPanel,
  ObservabilityPanelView,
  type RuntimeObservabilityLabels,
} from "@/components/ObservabilityPanel";
import {
  MobileRemoteShell,
  type MobileRemoteConnection,
  type MobileRemoteSessionStatus,
} from "@/components/host/MobileRemoteShell";
import { WebLoginPanel, type WebLoginStatus } from "@/components/host/WebLoginPanel";
import type { HostMode } from "@/components/host/hostMode";
import type { ObservabilityApi, ObservabilitySnapshot } from "@/lib/observability";
import "./design-system-showcase.css";

const WEB_LOGIN_STATES: readonly WebLoginStatus[] = [
  "signed-out",
  "submitting",
  "error",
  "signed-in",
];

const MOBILE_CONNECTION_STATES: readonly MobileRemoteConnection[] = [
  "connecting",
  "connected",
  "reconnecting",
  "offline",
  "unauthorized",
];

export const designSystemObservabilityLabels: RuntimeObservabilityLabels = {
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

const EMPTY_SNAPSHOT: ObservabilitySnapshot = {
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

const DATA_SNAPSHOT: ObservabilitySnapshot = {
  ...EMPTY_SNAPSHOT,
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
    traceId: "showcase-trace",
    spanId: "showcase-span",
    parentSpanId: null,
    name: "runtime.turn",
    startedAtMs: 1_700_000_000_000,
    durationMs: 480,
    ttftMs: 80,
    status: "ok",
    attributes: { surface: "showcase" },
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
    payload: { surface: "showcase" },
  }],
};

export interface DesignSystemShowcaseProps {
  hostMode?: HostMode;
  observabilityApi?: ObservabilityApi;
}

export function DesignSystemShowcase({
  hostMode = "desktop",
  observabilityApi,
}: DesignSystemShowcaseProps) {
  const observabilitySource = observabilityApi ? "adapter" : "fixture";
  return (
    <main className="design-system-showcase" data-host-mode={hostMode} data-observability-source={observabilitySource} data-testid="design-system-showcase">
      <header className="design-system-showcase__heading">
        <p className="design-system-showcase__eyebrow">KeenCode UI</p>
        <h1>宿主与运行状态展示</h1>
        <p>独立展示入口，不参与应用顶层装配；外层可切换主题与宿主模式。</p>
      </header>

      <section className="design-system-showcase__section" data-host-mode="desktop">
        <div className="design-system-showcase__section-heading">
          <h2>Desktop</h2>
          <span>本地桌面宿主</span>
        </div>
        <Card className="design-system-showcase__desktop-preview">
          <strong>KeenCode 工作台</strong>
          <span>本地会话、项目文件和终端由桌面宿主提供。</span>
        </Card>
      </section>

      <section className="design-system-showcase__section" data-host-mode="web">
        <div className="design-system-showcase__section-heading">
          <h2>Web</h2>
          <span>登录状态矩阵</span>
        </div>
        <div className="design-system-showcase__matrix">
          {WEB_LOGIN_STATES.map((status) => (
            <div className="design-system-showcase__item" key={status} data-state={status}>
              <span className="design-system-showcase__state-label">{status}</span>
              <WebLoginPanel
                hostMode="web"
                status={status}
                token="test-token"
                onTokenChange={() => {}}
                onSubmit={() => {}}
                errorMessage={status === "error" ? "示例：会话授权失败" : null}
              />
            </div>
          ))}
        </div>
      </section>

      <section className="design-system-showcase__section" data-host-mode="mobile-remote">
        <div className="design-system-showcase__section-heading">
          <h2>Mobile Remote</h2>
          <span>连接与会话状态矩阵</span>
        </div>
        <div className="design-system-showcase__matrix">
          {MOBILE_CONNECTION_STATES.map((connection) => (
            <div className="design-system-showcase__item" key={connection} data-state={connection}>
              <span className="design-system-showcase__state-label">{connection}</span>
              <MobileRemoteShell
                hostMode="mobile-remote"
                connection={connection}
                asking={connection === "connected"}
                session={{
                  title: "修复登录状态",
                  summary: "远程会话正在处理最近一次请求。",
                  status: connection === "connected" ? "waiting" : sessionStatusForConnection(connection),
                  updatedLabel: "刚刚更新",
                }}
                onReconnect={() => {}}
              >
                <p className="design-system-showcase__remote-copy">远程回复内容由上层会话投影传入。</p>
              </MobileRemoteShell>
            </div>
          ))}
        </div>
      </section>

      <section className="design-system-showcase__section" data-testid="observability-state-matrix">
        <div className="design-system-showcase__section-heading">
          <h2>Observability</h2>
          <span>加载、错误、空数据与数据态</span>
        </div>
        <div className="design-system-showcase__matrix design-system-showcase__matrix--observability">
          <div className="design-system-showcase__item" data-state="loading">
            <span className="design-system-showcase__state-label">loading</span>
            <ObservabilityPanelView labels={designSystemObservabilityLabels} snapshot={null} loading onRefresh={() => {}} onExport={() => {}} />
          </div>
          <div className="design-system-showcase__item" data-state="error">
            <span className="design-system-showcase__state-label">error</span>
            <ObservabilityPanelView labels={designSystemObservabilityLabels} snapshot={null} error="示例：观测服务暂不可用" onRefresh={() => {}} onExport={() => {}} />
          </div>
          <div className="design-system-showcase__item" data-state="empty">
            <span className="design-system-showcase__state-label">empty</span>
            <ObservabilityPanelView labels={designSystemObservabilityLabels} snapshot={EMPTY_SNAPSHOT} onRefresh={() => {}} onExport={() => {}} />
          </div>
          <div className="design-system-showcase__item" data-state="data">
            <span className="design-system-showcase__state-label">data</span>
            <ObservabilityPanelView labels={designSystemObservabilityLabels} snapshot={DATA_SNAPSHOT} onRefresh={() => {}} onExport={() => {}} />
          </div>
        </div>
        {observabilityApi ? (
          <div className="design-system-showcase__adapter-preview" data-state="adapter">
            <span className="design-system-showcase__state-label">adapter</span>
            <ObservabilityPanel labels={designSystemObservabilityLabels} api={observabilityApi} />
          </div>
        ) : null}
      </section>
    </main>
  );
}

function sessionStatusForConnection(connection: MobileRemoteConnection): MobileRemoteSessionStatus {
  if (connection === "offline" || connection === "unauthorized") return "failed";
  if (connection === "connecting" || connection === "reconnecting") return "running";
  return "idle";
}

export { DATA_SNAPSHOT as designSystemDataSnapshot, EMPTY_SNAPSHOT as designSystemEmptySnapshot };

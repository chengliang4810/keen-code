import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { ObservabilityPanelView } from "./ObservabilityPanel";
import {
  designSystemDataSnapshot,
  designSystemEmptySnapshot,
  designSystemObservabilityLabels,
} from "./design-system/DesignSystemShowcase";

describe("ObservabilityPanelView", () => {
  it("覆盖 loading、error、empty 和 data 展示态", () => {
    const loading = renderToStaticMarkup(
      <ObservabilityPanelView
        labels={designSystemObservabilityLabels}
        snapshot={null}
        loading
      />,
    );
    expect(loading).toContain("正在读取观测数据");
    expect(loading).not.toContain('data-testid="runtime-observability"');

    const error = renderToStaticMarkup(
      <ObservabilityPanelView
        labels={designSystemObservabilityLabels}
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
        labels={designSystemObservabilityLabels}
        snapshot={designSystemEmptySnapshot}
        onRefresh={vi.fn()}
        onExport={vi.fn()}
      />,
    );
    expect(empty).toContain("无启动阶段");
    expect(empty).toContain("无资源采样");
    expect(empty).toContain("无数据");

    const data = renderToStaticMarkup(
      <ObservabilityPanelView
        labels={designSystemObservabilityLabels}
        snapshot={designSystemDataSnapshot}
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
        labels={designSystemObservabilityLabels}
        snapshot={designSystemEmptySnapshot}
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

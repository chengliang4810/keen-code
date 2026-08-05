import React from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { TimelinePhase } from "@/lib/timelinePhases";
import { TimelinePhaseBlock } from "./TimelinePhaseBlock";

function phase(live: boolean): TimelinePhase {
  return {
    kind: "phase",
    id: "p-1",
    thoughts: [],
    startSi: 1,
    endSi: 2,
    live,
    errorCount: 0,
    runningCount: live ? 1 : 0,
    tools: [
      {
        kind: "tool",
        toolCallId: "read-1",
        title: "Read App.tsx",
        toolKind: "Read",
        status: "completed",
      },
      {
        kind: "tool",
        toolCallId: "bash-1",
        title: "pnpm test",
        toolKind: "Bash",
        status: live ? "running" : "completed",
        streaming: live,
      },
    ],
  };
}

describe("TimelinePhaseBlock", () => {
  it("活动工具组默认展开并在标题显示进度与当前动作", () => {
    const html = renderToString(
      React.createElement(TimelinePhaseBlock, {
        phase: phase(true),
        locale: "zh",
        messageStreaming: true,
      }),
    );

    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain("正在工作 · 1/2 步 · pnpm test");
    expect(html).toContain("lobe-timeline-phase__activity");
    expect(html).toContain("Read App.tsx");
  });

  it("历史中的已结束工具组默认折叠", () => {
    const html = renderToString(
      React.createElement(TimelinePhaseBlock, {
        phase: phase(false),
        locale: "zh",
      }),
    );

    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain("2 步");
    expect(html).not.toContain("Read App.tsx");
  });
});

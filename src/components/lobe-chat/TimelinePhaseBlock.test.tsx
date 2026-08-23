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
        input: '{"file_path":"src/App.tsx"}',
      },
      {
        kind: "tool",
        toolCallId: "bash-1",
        title: "pnpm test",
        toolKind: "Bash",
        status: live ? "running" : "completed",
        streaming: live,
        input: '{"command":"pnpm test"}',
      },
    ],
  };
}

describe("TimelinePhaseBlock", () => {
  it("活动工具组默认折叠并在标题显示当前命令", () => {
    const html = renderToString(
      React.createElement(TimelinePhaseBlock, {
        phase: phase(true),
        locale: "zh",
        messageStreaming: true,
      }),
    );

    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain("正在运行 pnpm test");
    expect(html).toContain("lobe-timeline-phase__activity");
    expect(html).not.toContain("Read App.tsx");
  });

  it("活动状态已提前结束时仍使用最后一条命令而非进行中占位", () => {
    const live = phase(true);
    live.tools[1]!.status = "completed";
    live.tools[1]!.streaming = false;
    live.runningCount = 0;
    const html = renderToString(
      React.createElement(TimelinePhaseBlock, {
        phase: live,
        locale: "zh",
        messageStreaming: true,
      }),
    );

    expect(html).toContain("正在运行 pnpm test");
    expect(html).not.toContain("进行中…");
  });

  it("历史中的已结束工具组默认折叠", () => {
    const html = renderToString(
      React.createElement(TimelinePhaseBlock, {
        phase: phase(false),
        locale: "zh",
      }),
    );

    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("lobe-timeline-phase__badge");
    expect(html).toContain("读取了文件、运行了命令");
    expect(html).not.toContain("Read App.tsx");
  });

  it("工具组不展示工具调用数量", () => {
    const phaseWithThought = phase(false);
    phaseWithThought.thoughts = ["检查现有实现"];
    const html = renderToString(
      React.createElement(TimelinePhaseBlock, {
        phase: phaseWithThought,
        locale: "zh",
      }),
    );

    expect(html).not.toContain("lobe-timeline-phase__badge");
    expect(html).not.toContain('aria-label="2 步"');
  });
});

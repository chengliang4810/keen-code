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

/** 构造 ACP 将主动取消表达为 failed + completionStatus 的工具段。 */
function cancelledTool(
  toolCallId: string,
  title: string,
  toolKind: string,
  input: string,
): TimelinePhase["tools"][number] {
  return {
    kind: "tool",
    toolCallId,
    title,
    toolKind,
    status: "failed",
    completionStatus: "cancelled",
    isError: true,
    // 取消结果即使带有迟到的流式标记，也不能回到运行态。
    streaming: true,
    input,
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

  it("单个工具失败时工具组标题保持中性色", () => {
    const failed = phase(false);
    failed.tools[1]!.status = "failed";
    failed.tools[1]!.isError = true;
    failed.errorCount = 1;
    const html = renderToString(
      React.createElement(TimelinePhaseBlock, {
        phase: failed,
        locale: "zh",
      }),
    );

    expect(html).toContain("1 失败");
    expect(html).not.toContain("is-error");
  });

  it("单独取消的工具组不计失败且不显示运行态", () => {
    const cancelled: TimelinePhase = {
      kind: "phase",
      id: "p-cancelled",
      thoughts: [],
      startSi: 1,
      endSi: 1,
      // 保留 live 以覆盖取消结果到达时的瞬态，不能回退成正在运行。
      live: true,
      errorCount: 0,
      runningCount: 0,
      tools: [
        cancelledTool(
          "cancel-1",
          "pnpm test",
          "Bash",
          '{"command":"pnpm test"}',
        ),
      ],
    };
    const html = renderToString(
      React.createElement(TimelinePhaseBlock, {
        phase: cancelled,
        locale: "zh",
        messageStreaming: true,
      }),
    );

    expect(html).toContain("1 已取消");
    expect(html).not.toContain("1 失败");
    expect(html).not.toContain("正在运行");
    expect(html).not.toContain("进行中");
  });

  it("失败与取消混合时分别统计，取消不会增加失败数", () => {
    const mixed: TimelinePhase = {
      kind: "phase",
      id: "p-mixed-outcome",
      thoughts: [],
      startSi: 1,
      endSi: 2,
      live: false,
      errorCount: 1,
      runningCount: 0,
      tools: [
        {
          kind: "tool",
          toolCallId: "failed-1",
          title: "Read App.tsx",
          toolKind: "Read",
          status: "failed",
          isError: true,
          streaming: false,
          input: '{"file_path":"src/App.tsx"}',
        },
        cancelledTool(
          "cancel-2",
          "pnpm test",
          "Bash",
          '{"command":"pnpm test"}',
        ),
      ],
    };
    const html = renderToString(
      React.createElement(TimelinePhaseBlock, {
        phase: mixed,
        locale: "zh",
      }),
    );

    expect(html).toContain("1 失败");
    expect(html).toContain("1 已取消");
    expect(html).not.toContain("2 失败");
    expect(html).not.toContain("正在运行");
    expect(html).not.toContain("进行中");
  });
});

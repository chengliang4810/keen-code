import { describe, expect, it } from "vitest";
import type { MessageSegment } from "./session";
import {
  buildTimelineUnits,
  isPhaseWorthy,
  phaseTitleModel,
} from "./timelinePhases";

function tool(
  id: string,
  title: string,
  status = "completed",
): Extract<MessageSegment, { kind: "tool" }> {
  return {
    kind: "tool",
    toolCallId: id,
    title,
    toolKind: "Read",
    status,
    streaming: status === "running",
  };
}

describe("timelinePhases", () => {
  it("isPhaseWorthy: 仅连续 ≥2 个工具成组", () => {
    expect(isPhaseWorthy(["plan"], [tool("a", "Read a")])).toBe(false);
    expect(isPhaseWorthy([], [tool("a", "a"), tool("b", "b")])).toBe(true);
    expect(isPhaseWorthy(["only think"], [])).toBe(false);
    expect(isPhaseWorthy([], [tool("a", "a")])).toBe(false);
  });

  it("子智能体工具始终保留为独立时间线位置", () => {
    const agent = tool("agent-1", "Agent");
    agent.toolKind = "Agent";
    const units = buildTimelineUnits([
      tool("read-1", "Read a"),
      agent,
      tool("read-2", "Read b"),
    ]);

    expect(units.map((unit) => unit.kind)).toEqual(["tool", "tool", "tool"]);
    expect(units[1]?.kind === "tool" && units[1].tool.toolCallId).toBe(
      "agent-1",
    );
  });

  it("closes phase when content starts (not at full turn end only)", () => {
    const segs: MessageSegment[] = [
      { kind: "thought", text: "**定位** 目录结构" },
      tool("t1", "Read a"),
      tool("t2", "Read b"),
      { kind: "content", text: "结论如下。" },
      { kind: "thought", text: "再查一遍" },
      tool("t3", "Read c"),
      { kind: "content", text: "补充。" },
    ];
    // Still streaming after first content would keep later work live — turn done:
    const units = buildTimelineUnits(segs, { streaming: false });
    expect(units.map((u) => u.kind)).toEqual([
      "thought",
      "phase",
      "content",
      "thought",
      "tool",
      "content",
    ]);
    const p0 = units[1]!;
    expect(p0.kind).toBe("phase");
    if (p0.kind === "phase") {
      expect(p0.live).toBe(false);
      expect(p0.tools).toHaveLength(2);
      expect(p0.thoughts).toEqual([]);
      const title = phaseTitleModel(p0);
      expect(title.stepCount).toBe(2);
    }
  });

  it("new thought after tools starts a new phase", () => {
    const segs: MessageSegment[] = [
      { kind: "thought", text: "round1" },
      tool("t1", "Read a"),
      { kind: "thought", text: "round2" },
      tool("t2", "Read b"),
      tool("t3", "Read c"),
    ];
    const units = buildTimelineUnits(segs, { streaming: false });
    expect(units.map((u) => u.kind)).toEqual([
      "thought",
      "tool",
      "thought",
      "phase",
    ]);
    if (units[3]!.kind === "phase") {
      expect(units[3]!.tools).toHaveLength(2);
      expect(units[3]!.thoughts).toEqual([]);
    }
  });

  it("trailing work stays live while streaming", () => {
    const segs: MessageSegment[] = [
      { kind: "thought", text: "**探索**" },
      tool("t1", "Read a", "completed"),
      tool("t2", "Read b", "running"),
    ];
    const live = buildTimelineUnits(segs, { streaming: true });
    expect(live).toHaveLength(2);
    expect(live[1]!.kind).toBe("phase");
    if (live[1]!.kind === "phase") {
      expect(live[1]!.live).toBe(true);
      expect(live[1]!.runningCount).toBe(1);
    }
    const done = buildTimelineUnits(segs.map((s) =>
      s.kind === "tool" ? { ...s, status: "completed", streaming: false } : s,
    ), { streaming: false });
    if (done[1]!.kind === "phase") {
      expect(done[1]!.live).toBe(false);
    }
  });

  it("single thought or single tool stays bare (not a phase chip)", () => {
    expect(
      buildTimelineUnits(
        [{ kind: "thought", text: "hmm" }, { kind: "content", text: "hi" }],
        { streaming: false },
      ).map((u) => u.kind),
    ).toEqual(["thought", "content"]);

    expect(
      buildTimelineUnits(
        [tool("only", "Read x"), { kind: "content", text: "ok" }],
        { streaming: false },
      ).map((u) => u.kind),
    ).toEqual(["tool", "content"]);
  });

  it("failed tools set errorCount for default expand", () => {
    const units = buildTimelineUnits(
      [
        { kind: "thought", text: "try" },
        tool("ok", "Read a"),
        {
          ...tool("bad", "Shell"),
          toolKind: "Execute",
          status: "failed",
          isError: true,
        },
      ],
      { streaming: false },
    );
    expect(units[1]!.kind).toBe("phase");
    if (units[1]!.kind === "phase") {
      expect(units[1]!.errorCount).toBe(1);
    }
  });

  it("history reconstruction thought→tools→content yields phase then content", () => {
    const segs: MessageSegment[] = [
      { kind: "thought", text: "**定位** 项目" },
      tool("t1", "Read a"),
      tool("t2", "Read b"),
      tool("t3", "Read c"),
      { kind: "content", text: "项目概览……" },
    ];
    const units = buildTimelineUnits(segs, { streaming: false });
    expect(units.map((u) => u.kind)).toEqual([
      "thought",
      "phase",
      "content",
    ]);
    if (units[1]!.kind === "phase") {
      expect(units[1]!.live).toBe(false);
      expect(units[1]!.tools).toHaveLength(3);
    }
  });
});

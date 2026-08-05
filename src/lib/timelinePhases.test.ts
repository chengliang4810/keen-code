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
  it("isPhaseWorthy: thought+tool or ≥2 tools", () => {
    expect(isPhaseWorthy(["plan"], [tool("a", "Read a")])).toBe(true);
    expect(isPhaseWorthy([], [tool("a", "a"), tool("b", "b")])).toBe(true);
    expect(isPhaseWorthy(["only think"], [])).toBe(false);
    expect(isPhaseWorthy([], [tool("a", "a")])).toBe(false);
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
      "phase",
      "content",
      "phase",
      "content",
    ]);
    const p0 = units[0]!;
    expect(p0.kind).toBe("phase");
    if (p0.kind === "phase") {
      expect(p0.live).toBe(false);
      expect(p0.tools).toHaveLength(2);
      expect(p0.thoughts[0]).toContain("定位");
      const title = phaseTitleModel(p0);
      expect(title.gist).toBeTruthy();
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
    expect(units.map((u) => u.kind)).toEqual(["phase", "phase"]);
    if (units[0]!.kind === "phase") {
      expect(units[0]!.tools).toHaveLength(1);
      expect(units[0]!.thoughts[0]).toBe("round1");
    }
    if (units[1]!.kind === "phase") {
      expect(units[1]!.tools).toHaveLength(2);
      expect(units[1]!.thoughts[0]).toBe("round2");
    }
  });

  it("trailing work stays live while streaming", () => {
    const segs: MessageSegment[] = [
      { kind: "thought", text: "**探索**" },
      tool("t1", "Read a", "completed"),
      tool("t2", "Read b", "running"),
    ];
    const live = buildTimelineUnits(segs, { streaming: true });
    expect(live).toHaveLength(1);
    expect(live[0]!.kind).toBe("phase");
    if (live[0]!.kind === "phase") {
      expect(live[0]!.live).toBe(true);
      expect(live[0]!.runningCount).toBe(1);
    }
    const done = buildTimelineUnits(segs.map((s) =>
      s.kind === "tool" ? { ...s, status: "completed", streaming: false } : s,
    ), { streaming: false });
    if (done[0]!.kind === "phase") {
      expect(done[0]!.live).toBe(false);
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
    expect(units[0]!.kind).toBe("phase");
    if (units[0]!.kind === "phase") {
      expect(units[0]!.errorCount).toBe(1);
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
    expect(units.map((u) => u.kind)).toEqual(["phase", "content"]);
    if (units[0]!.kind === "phase") {
      expect(units[0]!.live).toBe(false);
      expect(units[0]!.tools).toHaveLength(3);
    }
  });
});

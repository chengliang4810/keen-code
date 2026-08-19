import { describe, expect, it } from "vitest";
import type { MessageSegment } from "./session";
import {
  buildConversationTimelineUnits,
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
  it("keeps interleaved late thoughts above one continuous answer", () => {
    const units = buildConversationTimelineUnits(
      [
        { kind: "thought", text: "先分析。" },
        { kind: "content", text: "这是一个名为 KeenCode 的纯" },
        { kind: "thought", text: "我可以用简洁的方式回答。" },
        { kind: "content", text: "桌面端 AI 编码工具项目。" },
      ],
      { streaming: false },
    );

    expect(units.map((unit) => unit.kind)).toEqual(["thought", "content"]);
    expect(units[0]).toMatchObject({
      kind: "thought",
      text: "先分析。我可以用简洁的方式回答。",
    });
    expect(units[1]).toMatchObject({
      kind: "content",
      text: "这是一个名为 KeenCode 的纯桌面端 AI 编码工具项目。",
    });
  });

  it("keeps tools as boundaries between separate thought stages", () => {
    const units = buildConversationTimelineUnits([
      { kind: "thought", text: "阶段一" },
      { kind: "content", text: "正文前半" },
      tool("t1", "Read a"),
      { kind: "thought", text: "阶段二" },
      { kind: "content", text: "正文后半" },
    ]);

    expect(units.map((unit) => unit.kind)).toEqual([
      "thought",
      "tool",
      "thought",
      "content",
    ]);
    expect(units[0]).toMatchObject({ kind: "thought", text: "阶段一" });
    expect(units[2]).toMatchObject({ kind: "thought", text: "阶段二" });
    expect(units[3]).toMatchObject({
      kind: "content",
      text: "正文前半正文后半",
    });
  });

  it("marks only the currently arriving late-thought or content unit live", () => {
    const lateThought = buildConversationTimelineUnits(
      [
        { kind: "content", text: "正文" },
        { kind: "thought", text: "晚到思考" },
      ],
      { streaming: true },
    );
    expect(lateThought).toEqual([
      { kind: "thought", text: "晚到思考", si: 0, streaming: true },
      { kind: "content", text: "正文", si: 0, streaming: false },
    ]);

    const resumedContent = buildConversationTimelineUnits(
      [
        { kind: "content", text: "正文前半" },
        { kind: "thought", text: "思考" },
        { kind: "content", text: "正文后半" },
      ],
      { streaming: true },
    );
    expect(resumedContent[0]).toMatchObject({
      kind: "thought",
      streaming: false,
    });
    expect(resumedContent[1]).toMatchObject({
      kind: "content",
      text: "正文前半正文后半",
      streaming: true,
    });
  });

  it("isPhaseWorthy: 仅连续 ≥2 个工具成组", () => {
    expect(isPhaseWorthy(["plan"], [tool("a", "Read a")])).toBe(false);
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

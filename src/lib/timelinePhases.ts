/**
 * Display-layer projection: group thought + tool bursts into collapsible phases.
 *
 * Truth stays in MessageSegment[]; this only decides how the timeline renders.
 *
 * Phase boundaries (close previous work phase):
 * - content starts after thought/tool work
 * - new thought after tools (next reasoning round)
 * - turn ends (not streaming) — flush trailing work closed
 *
 * While streaming, the trailing work buffer stays "live" (expanded) until a
 * boundary closes it — merge happens at phase end, not only at final answer.
 */

import type { MessageSegment, MessageToolSegment } from "./session";
import { extractThinkingSummary } from "./thinkingSummary";

export interface TimelinePhase {
  kind: "phase";
  /** Stable key for React: start–end segment indices. */
  id: string;
  thoughts: string[];
  tools: MessageToolSegment[];
  startSi: number;
  endSi: number;
  /** Trailing open work while assistant still streaming. */
  live: boolean;
  errorCount: number;
  runningCount: number;
}

export type TimelineUnit =
  | TimelinePhase
  | {
      kind: "thought";
      text: string;
      si: number;
      streaming: boolean;
    }
  | {
      kind: "tool";
      tool: MessageToolSegment;
      si: number;
    }
  | {
      kind: "content";
      text: string;
      si: number;
      streaming: boolean;
    };

function toolRunning(t: MessageToolSegment): boolean {
  if (t.streaming) return true;
  const s = (t.status || "").toLowerCase();
  return s === "in_progress" || s === "pending" || s === "running" || s === "";
}

function toolFailed(t: MessageToolSegment): boolean {
  if (t.isError) return true;
  const s = (t.status || "").toLowerCase();
  return s === "failed" || s === "error" || s === "rejected" || s === "denied";
}

function phaseStats(tools: MessageToolSegment[]): {
  errorCount: number;
  runningCount: number;
} {
  let errorCount = 0;
  let runningCount = 0;
  for (const t of tools) {
    if (toolFailed(t)) errorCount += 1;
    if (toolRunning(t)) runningCount += 1;
  }
  return { errorCount, runningCount };
}

/**
 * Worth a collapsible phase chip (vs leaving as bare Thought / single tool row).
 * - thought + ≥1 tool
 * - ≥2 tools (with or without thought)
 */
export function isPhaseWorthy(
  thoughts: string[],
  tools: MessageToolSegment[],
): boolean {
  const hasThought = thoughts.some((t) => t.trim());
  if (tools.length >= 2) return true;
  if (hasThought && tools.length >= 1) return true;
  return false;
}

type WorkBuf = {
  thoughts: { text: string; si: number }[];
  tools: { tool: MessageToolSegment; si: number }[];
};

function emptyBuf(): WorkBuf {
  return { thoughts: [], tools: [] };
}

function bufStartSi(buf: WorkBuf): number {
  const a = buf.thoughts[0]?.si;
  const b = buf.tools[0]?.si;
  if (a == null) return b ?? 0;
  if (b == null) return a;
  return Math.min(a, b);
}

function bufEndSi(buf: WorkBuf): number {
  let end = 0;
  for (const t of buf.thoughts) end = Math.max(end, t.si);
  for (const t of buf.tools) end = Math.max(end, t.si);
  return end;
}

function bufEmpty(buf: WorkBuf): boolean {
  return buf.thoughts.length === 0 && buf.tools.length === 0;
}

/**
 * Project segments into display units with phase collapsing.
 */
export function buildTimelineUnits(
  segs: MessageSegment[],
  options: { streaming?: boolean; groupPhases?: boolean } = {},
): TimelineUnit[] {
  const streaming = !!options.streaming;
  if (options.groupPhases === false) {
    return segs.map((segment, si) => {
      if (segment.kind === "content") {
        return {
          kind: "content",
          text: segment.text,
          si,
          streaming: streaming && si === segs.length - 1,
        };
      }
      if (segment.kind === "thought") {
        return {
          kind: "thought",
          text: segment.text,
          si,
          streaming: streaming && si === segs.length - 1,
        };
      }
      return { kind: "tool", tool: segment, si };
    });
  }
  const out: TimelineUnit[] = [];
  let buf = emptyBuf();

  const emitBare = (b: WorkBuf, live: boolean) => {
    for (const th of b.thoughts) {
      if (!th.text.trim() && !(live && streaming)) continue;
      out.push({
        kind: "thought",
        text: th.text,
        si: th.si,
        streaming: live && streaming && th === b.thoughts[b.thoughts.length - 1] && b.tools.length === 0,
      });
    }
    for (const t of b.tools) {
      out.push({ kind: "tool", tool: t.tool, si: t.si });
    }
  };

  const flush = (live: boolean) => {
    if (bufEmpty(buf)) return;
    const thoughts = buf.thoughts.map((t) => t.text);
    const tools = buf.tools.map((t) => t.tool);
    if (isPhaseWorthy(thoughts, tools)) {
      const startSi = bufStartSi(buf);
      const endSi = bufEndSi(buf);
      const stats = phaseStats(tools);
      out.push({
        kind: "phase",
        id: `p-${startSi}-${endSi}`,
        thoughts: thoughts.filter((t) => t.trim()),
        tools,
        startSi,
        endSi,
        live,
        errorCount: stats.errorCount,
        runningCount: stats.runningCount,
      });
    } else {
      emitBare(buf, live);
    }
    buf = emptyBuf();
  };

  for (let si = 0; si < segs.length; si++) {
    const seg = segs[si]!;
    if (seg.kind === "content") {
      // Content closes any prior work phase (merge at phase end, not turn end).
      flush(false);
      out.push({
        kind: "content",
        text: seg.text,
        si,
        streaming: streaming && si === segs.length - 1,
      });
      continue;
    }
    if (seg.kind === "thought") {
      // New thought after tools → previous tool burst is a closed phase.
      if (buf.tools.length > 0) {
        flush(false);
      }
      buf.thoughts.push({ text: seg.text, si });
      continue;
    }
    // tool
    buf.tools.push({ tool: seg, si });
  }

  // Trailing work: live while streaming, closed when turn finished.
  flush(streaming);

  // Fix streaming flag on trailing bare thought when live phase wasn't used.
  if (streaming && out.length) {
    const last = out[out.length - 1]!;
    if (last.kind === "thought" && last === out[out.length - 1]) {
      last.streaming = true;
    }
    if (last.kind === "phase" && last.live) {
      // keep live
    }
  }

  return out;
}

/** One-line title pieces for a phase trigger (caller localizes). */
export function phaseTitleModel(phase: TimelinePhase): {
  gist: string | null;
  stepCount: number;
  errorCount: number;
  running: boolean;
  live: boolean;
} {
  const joined = phase.thoughts.join("\n\n");
  return {
    gist: extractThinkingSummary(joined),
    stepCount: phase.tools.length,
    errorCount: phase.errorCount,
    running: phase.live && phase.runningCount > 0,
    live: phase.live,
  };
}

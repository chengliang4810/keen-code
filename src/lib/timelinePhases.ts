/**
 * Display-layer projection: group consecutive tool bursts into collapsible phases.
 *
 * Truth stays in MessageSegment[]; this only decides how the timeline renders.
 *
 * Any text/thought segment closes the current tool phase. A single tool remains
 * a bare row; two or more consecutive tools become one phase.
 *
 * While streaming, the trailing work buffer stays "live" (expanded) until a
 * boundary closes it — merge happens at phase end, not only at final answer.
 */

import type { MessageSegment, MessageToolSegment } from "./session";

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
  _thoughts: string[],
  tools: MessageToolSegment[],
): boolean {
  return tools.length >= 2;
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
  let si = 0;
  while (si < segs.length) {
    const seg = segs[si]!;
    if (seg.kind === "content") {
      out.push({
        kind: "content",
        text: seg.text,
        si,
        streaming: streaming && si === segs.length - 1,
      });
      si += 1;
      continue;
    }
    if (seg.kind === "thought") {
      if (seg.text.trim() || (streaming && si === segs.length - 1)) {
        out.push({
          kind: "thought",
          text: seg.text,
          si,
          streaming: streaming && si === segs.length - 1,
        });
      }
      si += 1;
      continue;
    }

    const tools: MessageToolSegment[] = [seg];
    let endSi = si;
    while (endSi + 1 < segs.length && segs[endSi + 1]!.kind === "tool") {
      endSi += 1;
      tools.push(segs[endSi]! as MessageToolSegment);
    }
    if (tools.length < 2) {
      out.push({ kind: "tool", tool: seg, si });
    } else {
      const stats = phaseStats(tools);
      out.push({
        kind: "phase",
        id: `p-${si}`,
        thoughts: [],
        tools,
        startSi: si,
        endSi,
        live: streaming && endSi === segs.length - 1,
        errorCount: stats.errorCount,
        runningCount: stats.runningCount,
      });
    }
    si = endSi + 1;
  }
  return out;
}

/**
 * Conversation projection for providers that interleave answer and reasoning
 * chunks. Keep the stored segment timeline untouched, but render work first
 * and one continuous answer last.
 *
 * Removing content can make two thought chunks adjacent. Merge only those
 * chunks; tools remain hard phase boundaries between separate thought stages.
 */
export function buildConversationTimelineUnits(
  segs: MessageSegment[],
  options: { streaming?: boolean } = {},
): TimelineUnit[] {
  const work: MessageSegment[] = [];
  const content: string[] = [];
  let lastContentSi = -1;

  for (let si = 0; si < segs.length; si += 1) {
    const segment = segs[si]!;
    if (segment.kind === "content") {
      content.push(segment.text);
      lastContentSi = si;
      continue;
    }
    const previous = work[work.length - 1];
    if (segment.kind === "thought" && previous?.kind === "thought") {
      previous.text += segment.text;
    } else {
      work.push(segment.kind === "thought" ? { ...segment } : segment);
    }
  }

  const lastSegment = segs[segs.length - 1];
  const streaming = !!options.streaming;
  const units = buildTimelineUnits(work, {
    streaming: streaming && lastSegment?.kind !== "content",
  });
  const answer = content.join("");
  if (answer || (streaming && lastSegment?.kind === "content")) {
    units.push({
      kind: "content",
      text: answer,
      si: lastContentSi,
      streaming: streaming && lastSegment?.kind === "content",
    });
  }
  return units;
}

/** One-line title pieces for a phase trigger (caller localizes). */
export function phaseTitleModel(phase: TimelinePhase): {
  stepCount: number;
  errorCount: number;
  running: boolean;
  live: boolean;
} {
  return {
    stepCount: phase.tools.length,
    errorCount: phase.errorCount,
    running: phase.live && phase.runningCount > 0,
    live: phase.live,
  };
}

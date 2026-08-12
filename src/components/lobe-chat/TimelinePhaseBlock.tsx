/**
 * Collapsible work phase (CodePilot ToolActionsGroup–style).
 * Header: summary · caret right.
 * Body: single left rail with thinking + tool rows (flat, even spacing).
 */

import { useEffect, useMemo, useRef, useState } from "react";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import type { TimelinePhase } from "@/lib/timelinePhases";
import { phaseTitleModel } from "@/lib/timelinePhases";
import { extractThinkingSummary } from "@/lib/thinkingSummary";
import {
  summarizeCompletedTools,
  summarizeRunningTool,
} from "@/lib/toolDisplay";
import { IconChevronRight } from "@/components/icons";
import { Thinking } from "./Thinking";
import {
  TimelineToolRow,
} from "./TimelineToolRow";
import type { ResourceOpenTarget } from "@/components/ResourceViewer";

function buildPhaseTitle(
  phase: TimelinePhase,
  tr: ReturnType<typeof createT>,
  locale: Locale,
): string {
  const m = phaseTitleModel(phase);
  const n = m.stepCount;
  const e = m.errorCount;
  const gist = m.gist;

  if (m.live) {
    const current =
      [...phase.tools]
        .reverse()
        .find((tool) => {
          const status = (tool.status || "").toLowerCase();
          return (
            tool.streaming ||
            status === "" ||
            status === "in_progress" ||
            status === "pending" ||
            status === "running"
          );
        });
    return current
      ? summarizeRunningTool(
          {
            kind: current.toolKind,
            title: current.title,
            detail: current.detail,
            path: current.path,
            input: current.input,
          },
          locale,
        )
      : tr("timelinePhase.working");
  }
  if (m.running) {
    if (n > 0) return tr("timelinePhase.running", { n });
    return tr("timelinePhase.working");
  }
  const completedSummary = summarizeCompletedTools(
    phase.tools.map((tool) => ({
      kind: tool.toolKind,
      title: tool.title,
      detail: tool.detail,
      path: tool.path,
      input: tool.input,
    })),
    locale,
  );
  if (completedSummary && e > 0) {
    return `${completedSummary} · ${tr("timelinePhase.failed", { e })}`;
  }
  if (completedSummary) return completedSummary;
  if (n > 0 && e > 0) {
    return tr("timelinePhase.stepsWithErrors", { n, e });
  }
  if (n > 0) return tr("timelinePhase.steps", { n });
  if (gist) return gist;
  return tr("timelinePhase.working");
}

export function TimelinePhaseBlock({
  phase,
  locale,
  messageStreaming,
  onOpenResource,
}: {
  phase: TimelinePhase;
  locale: Locale;
  messageStreaming?: boolean;
  /** 从工具行打开文件或变更。 */
  onOpenResource?: (target: ResourceOpenTarget) => void;
}) {
  const tr = useMemo(() => createT(locale), [locale]);
  const title = useMemo(
    () => buildPhaseTitle(phase, tr, locale),
    [locale, phase, tr],
  );
  const [open, setOpen] = useState(phase.live);
  const previousLive = useRef(phase.live);

  useEffect(() => {
    if (previousLive.current && !phase.live) setOpen(false);
    if (!previousLive.current && phase.live) setOpen(true);
    previousLive.current = phase.live;
  }, [phase.live]);

  return (
    <div
      className={
        "lobe-timeline-phase" +
        (phase.live ? " is-live" : "") +
        (phase.errorCount > 0 ? " is-error" : "") +
        (open ? " is-open" : "")
      }
      data-testid="timeline-phase"
      data-phase-id={phase.id}
      data-live={phase.live ? "1" : "0"}
    >
      <button
        type="button"
        className="lobe-timeline-phase__trigger"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {phase.live ? (
          <span className="lobe-timeline-phase__activity" aria-hidden />
        ) : null}
        <span
          className={
            "lobe-timeline-phase__title" +
            (phase.errorCount > 0 ? " is-error" : "") +
            (phase.live || phase.runningCount > 0 ? " is-running" : "")
          }
        >
          {title}
        </span>
        <span
          className={
            "lobe-timeline-phase__caret" + (open ? " is-open" : "")
          }
          aria-hidden
        >
          <IconChevronRight size={12} />
        </span>
      </button>
      {open ? (
        <div className="lobe-timeline-rail">
          {phase.thoughts.map((text, i) => (
            <Thinking
              key={`${phase.id}-th-${i}`}
              locale={locale}
              thinking={
                !!(
                  phase.live &&
                  messageStreaming &&
                  i === phase.thoughts.length - 1 &&
                  phase.tools.length === 0
                )
              }
              content={text}
              processedLabel={(duration) =>
                tr("chat.processedFor", { duration })
              }
              triggerLabel={
                extractThinkingSummary(text) ?? tr("chat.thinking")
              }
            />
          ))}
          {phase.tools.map((tool) => (
            <TimelineToolRow
              key={`${phase.id}-tool-${tool.toolCallId}`}
              tool={tool}
              locale={locale}
              onOpenResource={onOpenResource}
            />
          ))}
        </div>
      ) : null}
    </div>
  );
}

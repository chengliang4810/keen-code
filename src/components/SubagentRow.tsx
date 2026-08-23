import { Button } from "@/components/ui/button";
import {
  IconAlertTriangle,
  IconCheck,
  IconLoader,
  IconSubagent,
} from "@/components/icons";
import { createT, type Locale } from "@/i18n";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import type { MessageSegment } from "@/lib/session";
import { useEffect, useState } from "react";

const EXCERPT_LIMIT = 110;

/** 子智能体列表使用的单行摘要，展示主 Agent 委派的任务。 */
export function subagentExcerpt(agent: AcpSubagentInfo): string {
  const content = agent.segments.find(
    (segment): segment is Extract<MessageSegment, { kind: "content" }> =>
      segment.kind === "content" && Boolean(segment.text.trim()),
  );
  const thought = agent.segments.find(
    (segment): segment is Extract<MessageSegment, { kind: "thought" }> =>
      segment.kind === "thought" && Boolean(segment.text.trim()),
  );
  const tool = agent.segments.find(
    (segment): segment is Extract<MessageSegment, { kind: "tool" }> =>
      segment.kind === "tool",
  );
  const raw =
    agent.prompt?.trim() ||
    content?.text.trim() ||
    agent.result?.trim() ||
    thought?.text.trim() ||
    tool?.title.trim() ||
    "";
  const flat = raw.replace(/\s+/g, " ");
  return flat.length > EXCERPT_LIMIT
    ? `${flat.slice(0, EXCERPT_LIMIT)}…`
    : flat;
}

function formatDuration(durationMs: number, locale: Locale): string {
  const seconds = Math.max(1, Math.floor(Math.max(0, durationMs) / 1_000));
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  if (locale === "en") {
    return minutes > 0 ? `${minutes}m ${rest}s` : `${seconds}s`;
  }
  return minutes > 0 ? `${minutes}分${rest}秒` : `${seconds}秒`;
}

function statusIcon(agent: AcpSubagentInfo) {
  if (agent.status === "running") {
    return <IconLoader size={15} className="summary-panel__spin" />;
  }
  if (agent.status === "failed") return <IconAlertTriangle size={15} />;
  return <IconCheck size={15} />;
}

export function SubagentRow({
  agent,
  locale,
  now,
  onClick,
  className = "",
}: {
  agent: AcpSubagentInfo;
  locale: Locale;
  now?: number;
  onClick: () => void;
  className?: string;
}) {
  const tr = createT(locale);
  const [clock, setClock] = useState(() => Date.now());
  useEffect(() => {
    if (now != null || agent.status !== "running") return;
    const timer = window.setInterval(() => setClock(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [agent.status, now]);
  const currentTime = now ?? clock;
  return (
    <Button
      type="button"
      className={`summary-panel__agent-row${className ? ` ${className}` : ""}`}
      onClick={onClick}
    >
      <span className={`summary-panel__agent-avatar is-${agent.status}`}>
        <IconSubagent size={18} />
      </span>
      <span className="summary-panel__agent-copy">
        <strong>{agent.agent_name}</strong>
        <small>
          {subagentExcerpt(agent) ||
            tr(
              agent.status === "running"
                ? "summary.subagents.processing"
                : "summary.subagents.noActivity",
            )}
        </small>
      </span>
      <span className="summary-panel__agent-meta">
        {agent.started_at > 0
          ? formatDuration(
              (agent.stopped_at ?? currentTime) - agent.started_at,
              locale,
            )
          : null}
        <span className={`is-${agent.status}`}>{statusIcon(agent)}</span>
      </span>
    </Button>
  );
}

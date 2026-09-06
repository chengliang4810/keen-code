import { Button } from "@/components/ui/button";
import {
  IconAlertTriangle,
  IconCheck,
  IconLoader,
  IconStop,
} from "@/components/icons";
import { AgentAvatar } from "@/components/AgentAvatar";
import { createT, type Locale } from "@/i18n";
import { agentNicknameLabel } from "@/lib/agentNicknames";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import { useEffect, useState } from "react";

const EXCERPT_LIMIT = 110;

/** 运行中展示最新活动，终态回到原始委派任务。 */
export function subagentExcerpt(agent: AcpSubagentInfo): string {
  const latest = [...agent.segments].reverse().find((segment) =>
    segment.kind === "tool" ? Boolean(segment.title.trim()) : Boolean(segment.text.trim()),
  );
  const activity = latest
    ? latest.kind === "tool"
      ? latest.title
      : latest.text
    : "";
  const raw = agent.status === "running" ? activity : agent.prompt?.trim() || "";
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
  if (agent.status === "interrupted") return <IconStop size={15} />;
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
  const displayName = agent.nickname
    ? agentNicknameLabel(agent.nickname, locale)
    : agent.agent_name;
  const headline = [displayName, agent.task_title].filter(Boolean).join(" · ");
  return (
    <Button
      type="button"
      className={`summary-panel__agent-row${className ? ` ${className}` : ""}`}
      onClick={onClick}
    >
      <span className={`summary-panel__agent-avatar is-${agent.status}`}>
        <AgentAvatar
          nickname={agent.nickname}
          agentId={agent.agent_id}
          size={26}
          status={agent.status}
        />
      </span>
      <span className="summary-panel__agent-copy">
        <span className="summary-panel__agent-identity">
          <strong title={headline}>{headline}</strong>
        </span>
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

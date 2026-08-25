import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
/** 轨迹台账：右侧停靠栏的会话记录流水（dsh Trajectory 台账的本地化版本）。 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  IconAlertTriangle,
  IconArrowsMinimize,
  IconBrain,
  IconCheck,
  IconChevronDown,
  IconChevronRight,
  IconListTree,
  IconLoader,
  IconMinimize,
  IconMaximize,
  IconRefresh,
  IconSearch,
  IconStop,
  IconSubagent,
  IconSummary,
  IconUser,
} from "@/components/icons";
import { createT, type Locale } from "@/i18n";
import { AgentAvatar } from "@/components/AgentAvatar";
import { OverlayScroll } from "@/components/OverlayScroll";
import { Tip } from "@/components/ui/tooltip";
import { formatTurnLatency } from "@/components/lobe-chat/TurnMetrics";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import { localizeUiError, type ChatMessage } from "@/lib/session";
import type {
  TrajectoryRecord,
  TrajectoryRecordKind,
  TrajectoryStats,
} from "@/lib/trajectory";
import {
  buildTrajectoryRecords,
  compactTrajectoryDetail,
  filterTrajectoryRecords,
  summarizeTrajectory,
  trajectorySingleLine,
} from "@/lib/trajectory";

export interface TrajectoryLiveSource {
  /** 当前正在查看的会话标识。 */
  sessionId: string | null;
  /** 当前查看会话的标题。 */
  title?: string | null;
  messages: ChatMessage[];
  subagents: AcpSubagentInfo[];
}

export interface TrajectoryLedgerProps {
  locale: Locale;
  /** 菜单「查看轨迹」指定的会话；为空时跟随当前查看的会话。 */
  sessionId: string | null;
  /** 台账展示的会话标题。 */
  title: string | null;
  live: TrajectoryLiveSource | null;
  onLoadMessages: (sessionId: string) => Promise<ChatMessage[]>;
}

interface StoredState {
  loading: boolean;
  error: string | null;
  messages: ChatMessage[];
}

type RenderItem =
  | { type: "turn-header"; turn: number; collapsed: boolean; stats: TrajectoryStats }
  | { type: "record"; record: TrajectoryRecord };

function kindIcon(record: TrajectoryRecord, size: number): ReactNode {
  switch (record.kind) {
    case "user":
      return <IconUser size={size} />;
    case "assistant":
      return <IconSummary size={size} />;
    case "thinking":
      return <IconBrain size={size} />;
    case "tool":
      return <IconListTree size={size} />;
    case "subagent":
      return record.subagent ? (
        <AgentAvatar
          nickname={record.subagent.nickname}
          agentId={record.subagent.agent_id}
          size={size + 3}
          status={record.subagent.status}
        />
      ) : (
        <IconSubagent size={size} />
      );
    case "compacted":
      return <IconArrowsMinimize size={size} />;
    case "error":
      return <IconAlertTriangle size={size} />;
    case "cancelled":
      return <IconStop size={size} />;
  }
}

function statusIcon(status: TrajectoryRecord["status"], size: number): ReactNode {
  if (status === "running") {
    return <IconLoader size={size} className="rp-traj__spin" />;
  }
  if (status === "failed") return <IconAlertTriangle size={size} />;
  return <IconCheck size={size} />;
}

function formatTokens(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 10_000) return `${Math.round(value / 1_000)}k`;
  return value.toLocaleString("en-US");
}

export function TrajectoryLedger({
  locale,
  sessionId,
  title,
  live,
  onLoadMessages,
}: TrajectoryLedgerProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const resolvedSessionId = sessionId ?? live?.sessionId ?? null;
  const isLive =
    resolvedSessionId != null && resolvedSessionId === live?.sessionId;

  const [query, setQuery] = useState("");
  const [collapsedTurns, setCollapsedTurns] = useState<Set<number>>(new Set());
  const [expandedKey, setExpandedKey] = useState<string | null>(null);
  const [reloadNonce, setReloadNonce] = useState(0);
  const [stored, setStored] = useState<StoredState>({
    loading: false,
    error: null,
    messages: [],
  });
  const loadSeq = useRef(0);

  // 非当前会话：从缓存 / 持久化消息加载静态轨迹。
  useEffect(() => {
    if (isLive || !resolvedSessionId) return;
    const seq = ++loadSeq.current;
    setStored({ loading: true, error: null, messages: [] });
    onLoadMessages(resolvedSessionId)
      .then((messages) => {
        if (seq !== loadSeq.current) return;
        setStored({ loading: false, error: null, messages });
      })
      .catch((error: unknown) => {
        if (seq !== loadSeq.current) return;
        setStored({
          loading: false,
          error: localizeUiError(error, locale),
          messages: [],
        });
      });
  }, [isLive, onLoadMessages, reloadNonce, resolvedSessionId]);

  const messages = isLive ? (live?.messages ?? []) : stored.messages;
  const subagents = isLive ? (live?.subagents ?? []) : [];

  const records = useMemo(
    () => buildTrajectoryRecords(messages, subagents, locale),
    [locale, messages, subagents],
  );
  const filtered = useMemo(
    () => filterTrajectoryRecords(records, query),
    [records, query],
  );
  const stats = useMemo(() => summarizeTrajectory(filtered), [filtered]);

  const turnNumbers = useMemo(
    () =>
      Array.from(
        new Set(filtered.map((record) => record.turn).filter((turn) => turn > 0)),
      ),
    [filtered],
  );
  const allTurnsCollapsed =
    turnNumbers.length > 0 &&
    turnNumbers.every((turn) => collapsedTurns.has(turn));

  const toggleTurn = useCallback((turn: number) => {
    setCollapsedTurns((prev) => {
      const next = new Set(prev);
      if (next.has(turn)) next.delete(turn);
      else next.add(turn);
      return next;
    });
  }, []);

  const toggleAllTurns = useCallback(() => {
    setCollapsedTurns((prev) => {
      const current = Array.from(
        new Set(records.map((record) => record.turn).filter((t) => t > 0)),
      );
      const allCollapsed = current.every((turn) => prev.has(turn));
      return allCollapsed ? new Set() : new Set(current);
    });
  }, [records]);

  const toggleRecord = useCallback((key: string) => {
    setExpandedKey((prev) => (prev === key ? null : key));
  }, []);

  const kindLabels = useMemo<Record<TrajectoryRecordKind, string>>(
    () => ({
      user: tr("trajectory.kind.user"),
      assistant: tr("trajectory.kind.assistant"),
      thinking: tr("trajectory.kind.thinking"),
      tool: tr("trajectory.kind.tool"),
      subagent: tr("trajectory.kind.subagent"),
      compacted: tr("trajectory.kind.compacted"),
      error: tr("trajectory.kind.error"),
      cancelled: tr("trajectory.kind.cancelled"),
    }),
    [tr],
  );

  // 台账渲染序列：轮次头 + 记录行；折叠的轮次仅渲染摘要头。
  const renderItems = useMemo<RenderItem[]>(() => {
    const items: RenderItem[] = [];
    let currentTurn: number | null = null;
    for (let i = 0; i < filtered.length; i += 1) {
      const record = filtered[i]!;
      if (record.turn > 0 && record.turn !== currentTurn) {
        currentTurn = record.turn;
        const collapsed = collapsedTurns.has(record.turn);
        if (collapsed) {
          const turnRecords: TrajectoryRecord[] = [];
          for (let k = i; k < filtered.length && filtered[k]!.turn === record.turn; k += 1) {
            turnRecords.push(filtered[k]!);
          }
          items.push({
            type: "turn-header",
            turn: record.turn,
            collapsed,
            stats: summarizeTrajectory(turnRecords),
          });
          i += turnRecords.length - 1;
          continue;
        }
        items.push({
          type: "turn-header",
          turn: record.turn,
          collapsed,
          stats: summarizeTrajectory([]),
        });
      }
      items.push({ type: "record", record });
    }
    return items;
  }, [collapsedTurns, filtered]);

  const renderDetailSection = (
    label: string,
    value: string | null | undefined,
  ) => {
    const text = compactTrajectoryDetail(value);
    if (!text) return null;
    return (
      <section className="rp-traj-detail__section">
        <span className="rp-traj-detail__label">{label}</span>
        <pre>{text}</pre>
      </section>
    );
  };

  const renderMetrics = (record: TrajectoryRecord) => {
    const metrics = record.metrics;
    if (!metrics) return null;
    const chips: string[] = [];
    const push = (key: Parameters<typeof tr>[0], value: string | null) => {
      if (value) chips.push(tr(key, { value }));
    };
    push(
      "chat.turnMetrics.acknowledged",
      formatTurnLatency(metrics.sendAcknowledgementMs ?? Number.NaN),
    );
    push(
      "chat.turnMetrics.firstSse",
      formatTurnLatency(metrics.timeToFirstSseMs ?? Number.NaN),
    );
    push(
      "chat.turnMetrics.firstVisible",
      formatTurnLatency(metrics.timeToFirstVisibleTokenMs ?? Number.NaN),
    );
    push(
      "chat.turnMetrics.completed",
      formatTurnLatency(metrics.totalMs ?? Number.NaN),
    );
    if (typeof metrics.inputTokens === "number") {
      chips.push(
        tr("trajectory.metrics.inputTokens", {
          value: formatTokens(metrics.inputTokens),
        }),
      );
    }
    if (typeof metrics.cacheReadTokens === "number") {
      chips.push(
        tr("trajectory.metrics.cacheRead", {
          value: formatTokens(metrics.cacheReadTokens),
        }),
      );
    }
    if (typeof metrics.cacheCreationTokens === "number") {
      chips.push(
        tr("trajectory.metrics.cacheCreation", {
          value: formatTokens(metrics.cacheCreationTokens),
        }),
      );
    }
    if (!chips.length) return null;
    return (
      <section className="rp-traj-detail__section">
        <span className="rp-traj-detail__label">
          {tr("trajectory.detail.metrics")}
        </span>
        <div className="rp-traj-metrics">
          {chips.map((chip) => (
            <span className="rp-traj-metrics__chip" key={chip}>
              {chip}
            </span>
          ))}
        </div>
      </section>
    );
  };

  const renderRecordDetail = (record: TrajectoryRecord): ReactNode => {
    const facts: Array<[string, string]> = [];
    if (record.toolKind)
      facts.push([tr("trajectory.detail.toolKind"), record.toolKind]);
    if (record.path) facts.push([tr("trajectory.detail.path"), record.path]);
    if (record.createdAt)
      facts.push([
        tr("trajectory.detail.startedAt"),
        new Date(record.createdAt).toLocaleString(),
      ]);
    if (record.durationMs != null) {
      const duration = formatTurnLatency(record.durationMs);
      if (duration)
        facts.push([tr("trajectory.detail.duration"), duration]);
    }
    if (record.subagent) {
      const toolCalls = record.subagent.segments.filter(
        (segment) => segment.kind === "tool",
      ).length;
      facts.push([
        tr("trajectory.detail.toolCalls"),
        String(toolCalls),
      ]);
    }
    if (record.compactMeta) {
      facts.push([
        tr("trajectory.compact.trigger"),
        record.compactMeta.trigger,
      ]);
      if (record.compactMeta.tokensBefore != null) {
        const tokens =
          record.compactMeta.tokensAfter != null
            ? `${formatTokens(record.compactMeta.tokensBefore)} → ${formatTokens(
                record.compactMeta.tokensAfter,
              )}`
            : formatTokens(record.compactMeta.tokensBefore);
        facts.push([tr("trajectory.detail.tokens"), tokens]);
      }
    }
    return (
      <div className="rp-traj-detail">
        {renderMetrics(record)}
        {renderDetailSection(tr("trajectory.detail.input"), record.input)}
        {renderDetailSection(tr("trajectory.detail.thinking"), record.thinking)}
        {renderDetailSection(
          tr("trajectory.detail.output"),
          record.output ??
            (record.compactMeta?.summaryPreview
              ? record.compactMeta.summaryPreview
              : record.subagent?.result),
        )}
        {record.attachments?.length ? (
          <section className="rp-traj-detail__section">
            <span className="rp-traj-detail__label">
              {tr("trajectory.detail.attachments")}
            </span>
            <ul className="rp-traj-attachments">
              {record.attachments.map((attachment) => (
                <li key={attachment.path}>{attachment.name}</li>
              ))}
            </ul>
          </section>
        ) : null}
        {facts.length ? (
          <dl className="rp-traj-facts">
            {facts.map(([label, value]) => (
              <div className="rp-traj-facts__row" key={label}>
                <dt>{label}</dt>
                <dd>{value}</dd>
              </div>
            ))}
          </dl>
        ) : null}
      </div>
    );
  };

  const renderRecord = (record: TrajectoryRecord) => {
    const expanded = expandedKey === record.key;
    const resultPreview =
      record.kind === "tool" && record.output
        ? trajectorySingleLine(record.output, 80)
        : "";
    return (
      <div
        className="rp-traj-row"
        data-kind={record.kind}
        data-status={record.status}
        key={record.key}
      >
        <Button
          type="button"
          className="rp-traj-row__main"
          aria-expanded={expanded}
          onClick={() => toggleRecord(record.key)}
        >
          <span className="rp-traj-kind" aria-hidden>
            {kindIcon(record, 13)}
          </span>
          <span className="rp-traj-kind__label">{kindLabels[record.kind]}</span>
          <span className="rp-traj-index">#{record.index}</span>
          <span className="rp-traj-title" title={record.title}>
            {record.title}
          </span>
          <span className="rp-traj-meta">
            {typeof record.durationMs === "number" && record.durationMs > 0 ? (
              <span className="rp-traj-duration">
                {formatTurnLatency(record.durationMs)}
              </span>
            ) : null}
            <span className="rp-traj-status" aria-hidden>
              {statusIcon(record.status, 13)}
            </span>
            <span className="rp-traj-chev" aria-hidden>
              {expanded ? (
                <IconChevronDown size={12} />
              ) : (
                <IconChevronRight size={12} />
              )}
            </span>
          </span>
        </Button>
        {resultPreview ? (
          <div className="rp-traj-result" aria-hidden>
            → {resultPreview}
          </div>
        ) : record.kind === "tool" && record.status !== "running" ? (
          <div className="rp-traj-result rp-traj-result--empty" aria-hidden>
            → {tr("trajectory.noOutput")}
          </div>
        ) : null}
        {expanded ? renderRecordDetail(record) : null}
      </div>
    );
  };

  let body: ReactNode;
  if (!resolvedSessionId) {
    body = (
      <div className="rp__empty-state">
        <div className="rp__empty-title">{tr("trajectory.noSession")}</div>
        <div className="rp__empty-desc">{tr("trajectory.emptyHint")}</div>
      </div>
    );
  } else if (!isLive && stored.loading) {
    body = (
      <div className="rp__empty-state rp__empty-state--sm">
        <div className="rp__empty-desc">{tr("resources.loading")}</div>
      </div>
    );
  } else if (!isLive && stored.error) {
    body = (
      <div className="rp__empty-state rp__empty-state--sm">
        <div className="rp__empty-desc">{tr("trajectory.loadFailed")}</div>
        <Button
          type="button"
          className="btn btn--ghost"
          onClick={() => setReloadNonce((n) => n + 1)}
        >
          {tr("trajectory.retry")}
        </Button>
      </div>
    );
  } else if (records.length === 0) {
    body = (
      <div className="rp__empty-state">
        <div className="rp__empty-title">{tr("trajectory.empty")}</div>
        <div className="rp__empty-desc">{tr("trajectory.emptyHint")}</div>
      </div>
    );
  } else if (filtered.length === 0) {
    body = (
      <div className="rp__empty-state rp__empty-state--sm">
        <div className="rp__empty-desc">{tr("trajectory.empty")}</div>
      </div>
    );
  } else {
    body = (
      <OverlayScroll className="rp-traj-scroll">
        <div className="rp-traj-list" role="list">
          {renderItems.map((item) =>
            item.type === "turn-header" ? (
              <Button
                type="button"
                className="rp-traj-turn"
                key={`turn-${item.turn}`}
                aria-expanded={!item.collapsed}
                onClick={() => toggleTurn(item.turn)}
              >
                <span className="rp-traj-turn__chev" aria-hidden>
                  {item.collapsed ? (
                    <IconChevronRight size={12} />
                  ) : (
                    <IconChevronDown size={12} />
                  )}
                </span>
                <span className="rp-traj-turn__label">
                  {tr("trajectory.turn", { n: item.turn })}
                </span>
                {item.collapsed ? (
                  <span className="rp-traj-turn__summary">
                    {tr("trajectory.turnSummary", {
                      records: item.stats.total,
                      tools: item.stats.tools,
                    })}
                  </span>
                ) : null}
              </Button>
            ) : (
              renderRecord(item.record)
            ),
          )}
        </div>
      </OverlayScroll>
    );
  }

  return (
    <div className="rp-trajectory" data-testid="trajectory-ledger">
      {title ? (
        <div className="rp-traj-session" title={title}>
          {title}
        </div>
      ) : null}
      <div className="rp-traj-toolbar">
        <div className="rp-traj-search">
          <IconSearch size={14} />
          <Input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={tr("trajectory.searchPh")}
            aria-label={tr("trajectory.searchPh")}
            spellCheck={false}
          />
        </div>
        <Tip
          label={
            allTurnsCollapsed
              ? tr("trajectory.expandTurns")
              : tr("trajectory.collapseTurns")
          }
        >
          <Button
            type="button"
            className={
              "rp-traj-tool-btn" + (allTurnsCollapsed ? " is-on" : "")
            }
            onClick={toggleAllTurns}
            aria-label={
              allTurnsCollapsed
                ? tr("trajectory.expandTurns")
                : tr("trajectory.collapseTurns")
            }
          >
            {allTurnsCollapsed ? <IconMaximize size={14} /> : <IconMinimize size={14} />}
          </Button>
        </Tip>
        {!isLive ? (
          <Tip label={tr("trajectory.refresh")}>
            <Button
              type="button"
              className="rp-traj-tool-btn"
              onClick={() => setReloadNonce((n) => n + 1)}
              aria-label={tr("trajectory.refresh")}
            >
              <IconRefresh size={14} />
            </Button>
          </Tip>
        ) : null}
      </div>
      {body}
      {records.length > 0 ? (
        <div className="rp-traj-footer" role="status">
          <span>
            {tr("trajectory.stats", {
              records: stats.total,
              tools: stats.tools,
              turns: stats.turns,
            })}
          </span>
          {stats.inputTokens != null ? (
            <span>
              {tr("trajectory.footerTokens", {
                input: formatTokens(stats.inputTokens),
                cache:
                  stats.cacheReadTokens != null
                    ? formatTokens(stats.cacheReadTokens)
                    : "—",
              })}
            </span>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

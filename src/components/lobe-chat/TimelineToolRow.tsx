import { Button } from "@/components/ui/button";
/**
 * Inline tool step on the assistant timeline (stream order).
 * Quiet red mark on failure; no bottom activity dump.
 */

import { useState } from "react";
import type { Locale } from "@/i18n";
import type {
  ChatMessage,
  MessageSegment,
  MessageToolSegment,
} from "@/lib/session";
import {
  isToolStepMessage,
  parseToolStepContent,
  toolStepDisplayTitle,
} from "@/lib/session";
import {
  classifyToolKind,
  isGoalToolName,
  isPlanToolName,
  parseToolInput,
  summarizeToolDisplay,
} from "@/lib/toolDisplay";
import { pathBasename } from "@/lib/filePath";
import { normalizeTaskStatus } from "@/lib/sessionTasks";
import {
  IconChevronDown,
  IconChevronRight,
  IconCheck,
  IconCode,
  IconEdit,
  IconExternalLink,
  IconFileText,
  IconFolder,
  IconPuzzle,
  IconSearch,
  IconUser,
} from "@/components/icons";
import type { AcpStructuredToolResult } from "@/lib/acp/types";
import { StructuredToolResultView } from "@/components/StructuredToolResultView";
import type { ResourceOpenTarget } from "@/components/ResourceViewer";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import { AgentAvatar } from "@/components/AgentAvatar";
import { agentNicknameLabel } from "@/lib/agentNicknames";
import {
  isToolSegmentFailed,
  isToolSegmentRunning,
} from "@/lib/toolSegmentStatus";

/** 在读取文件名后显示请求的行号范围。 */
function readPathLabel(path: string, offset?: number, limit?: number): string {
  const name = pathBasename(path);
  if (!name || !limit) return name;
  const start = offset ?? 1;
  return `${name}:${start}\u2013${start + limit - 1}`;
}

type TimelineToolCategory =
  | "folder"
  | "read"
  | "search"
  | "edit"
  | "command"
  | "ask-user"
  | "tool-search"
  | "skill-load"
  | "skill-search"
  | "web-search"
  | "web-fetch"
  | "tool-execute"
  | "wait-agent"
  | "other";

/**
 * ACP `kind` is a standard category while `title` is often the real tool
 * name. The latter must win for names such as `folder_operations`, whose
 * wire kind is the generic `edit` category.
 */
function timelineToolCategory(tool: MessageToolSegment): TimelineToolCategory {
  const title = (tool.title || "")
    .toLowerCase()
    .trim()
    .replace(/[\s./-]+/g, "_");
  const kind = (tool.toolKind || "")
    .toLowerCase()
    .trim()
    .replace(/[\s./-]+/g, "_");

  const categoryFor = (value: string): TimelineToolCategory => {
    if (value === "ask_user_question" || value === "askuserquestion") {
      return "ask-user";
    }
    if (value === "search_extra_tools" || value === "searchextratools") {
      return "tool-search";
    }
    if (value === "skill_tool" || value === "skilltool") {
      return "skill-load";
    }
    if (value === "discover_skills_tool" || value === "discoverskillstool") {
      return "skill-search";
    }
    if (value === "web_search" || value === "websearch") {
      return "web-search";
    }
    if (value === "web_fetch" || value === "webfetch") {
      return "web-fetch";
    }
    if (value === "execute_extra_tool" || value === "executeextratool") {
      return "tool-execute";
    }
    if (value === "wait_agent" || value === "waitagent") {
      return "wait-agent";
    }
    if (value.includes("folder_operations")) {
      return "folder";
    }
    if (
      value.includes("grep") ||
      value.includes("glob") ||
      value.includes("search")
    ) {
      return "search";
    }
    if (
      value.includes("bash") ||
      value.includes("shell") ||
      value.includes("exec") ||
      value.includes("terminal") ||
      value.includes("command")
    ) {
      return "command";
    }
    if (value.includes("read")) return "read";
    if (
      value.includes("edit") ||
      value.includes("write") ||
      value.includes("patch")
    ) {
      return "edit";
    }
    return "other";
  };

  const titleCategory = categoryFor(title);
  return titleCategory === "other" ? categoryFor(kind) : titleCategory;
}

/** 判断是否是计划/Todo 更新工具。 */
export function isPlanTool(tool: MessageToolSegment): boolean {
  return isPlanToolName(tool.toolKind, tool.title);
}

/** 判断是否是由输入框上方目标栏承载的 Goal 工具。 */
export function isGoalTool(tool: MessageToolSegment): boolean {
  return isGoalToolName(tool.toolKind, tool.title);
}

/** 判断工具是否已有输入框上方的专用状态界面。 */
export function isComposerStateTool(tool: MessageToolSegment): boolean {
  return isPlanTool(tool) || isGoalTool(tool);
}

function toolSummary(seg: MessageToolSegment): string {
  const display = summarizeToolDisplay({
    kind: seg.toolKind,
    title: seg.title,
    detail: seg.detail,
    path: seg.path,
  });
  return display.summary || seg.title || seg.toolKind || seg.toolCallId;
}

/** 从 WaitAgent 结果中的线程标识精确还原其等待的子任务标题。 */
export function waitAgentTaskTitles(
  tool: MessageToolSegment,
  subagents: readonly AcpSubagentInfo[],
): string[] {
  if (isToolSegmentRunning(tool)) {
    return subagents
      .filter((agent) => agent.status === "running")
      .map((agent) => agent.task_title?.trim() || agent.agent_name)
      .filter((title): title is string => !!title);
  }
  const raw = tool.output || tool.detail || "";
  try {
    const result = JSON.parse(raw) as {
      running_agents?: Array<{ child_thread_id?: unknown }>;
    };
    const ids = (result.running_agents || [])
      .map((agent) => agent.child_thread_id)
      .filter((id): id is string => typeof id === "string" && !!id);
    return ids.flatMap((id) => {
      const agent = subagents.find((candidate) => candidate.agent_id === id);
      if (!agent) return [];
      return [agent.task_title?.trim() || agent.agent_name].filter(Boolean);
    });
  } catch {
    return [];
  }
}

/** 读取 WaitAgent 已有的结束原因，不从工具状态推测。 */
export function waitAgentOutcome(tool: MessageToolSegment): string | null {
  try {
    const result = JSON.parse(tool.output || tool.detail || "") as {
      outcome?: unknown;
    };
    return typeof result.outcome === "string" ? result.outcome : null;
  } catch {
    return null;
  }
}

/** 将 Agent 工具调用关联到运行时登记的子智能体。 */
export function subagentForTool(
  tool: MessageToolSegment,
  subagents: readonly AcpSubagentInfo[],
): AcpSubagentInfo | null {
  if (classifyToolKind(tool.toolKind, tool.title) !== "subagent") return null;
  const evidence = [
    tool.input,
    tool.output,
    tool.detail,
    tool.structuredResult?.output,
  ]
    .filter(Boolean)
    .join("\n");
  const byId = subagents.find((agent) => evidence.includes(agent.agent_id));
  if (byId) return byId;

  let requestedType = "";
  try {
    const input = JSON.parse(tool.input || "{}") as Record<string, unknown>;
    requestedType =
      typeof input.subagent_type === "string"
        ? input.subagent_type.trim()
        : "";
  } catch {
    /* 非 JSON 输入只能依赖 child_thread_id。 */
  }
  const candidates = requestedType
    ? subagents.filter((agent) => agent.agent_name === requestedType)
    : subagents;
  if (candidates.length !== 1) return null;
  return candidates[0] ?? null;
}

/** 每个子 Agent 只让最后一张生命周期卡片表达当前运行状态。 */
export function latestSubagentToolCallIds(
  segments: readonly MessageSegment[],
  subagents: readonly AcpSubagentInfo[],
): Set<string> {
  const latestByAgent = new Map<string, string>();
  for (const segment of segments) {
    if (segment.kind !== "tool") continue;
    const agent = subagentForTool(segment, subagents);
    if (agent) latestByAgent.set(agent.agent_id, segment.toolCallId);
  }
  return new Set(latestByAgent.values());
}

function subagentCardFields(tool: MessageToolSegment): {
  description: string;
  subagentType: string;
} {
  try {
    const input = JSON.parse(tool.input || "{}") as Record<string, unknown>;
    const description = [input.description, input.message].find(
      (value): value is string => typeof value === "string" && !!value.trim(),
    );
    return {
      description: description?.trim() || "",
      subagentType:
        typeof input.subagent_type === "string"
          ? input.subagent_type.trim()
          : "",
    };
  } catch {
    return { description: "", subagentType: "" };
  }
}

function SubagentTimelineCard({
  agent,
  tool,
  locale,
  current,
  failed,
  onClick,
}: {
  agent: AcpSubagentInfo | null;
  tool: MessageToolSegment;
  locale: Locale;
  current: boolean;
  failed: boolean;
  onClick?: () => void;
}) {
  const fields = subagentCardFields(tool);
  const nickname = agent?.nickname
    ? agentNicknameLabel(agent.nickname, locale)
    : locale === "zh"
      ? "子 Agent"
      : "Sub-agent";
  const subagentType = fields.subagentType || agent?.agent_name || "Agent";
  const description =
    fields.description ||
    (locale === "zh" ? "未提供任务标题" : "Untitled task");
  const status = failed
    ? "failed"
    : current
      ? agent?.status || (isToolSegmentRunning(tool) ? "running" : "done")
      : "history";
  const statusLabel =
    status === "running"
      ? locale === "zh"
        ? "运行中"
        : "Running"
      : status === "done"
        ? locale === "zh"
          ? "已完成"
          : "Completed"
        : status === "failed"
          ? locale === "zh"
            ? "失败"
            : "Failed"
          : locale === "zh"
            ? "历史记录"
            : "History";

  const content = (
    <>
      <span className={`lobe-subagent-card__avatar is-${status}`} aria-hidden>
        <AgentAvatar
          nickname={agent?.nickname ?? null}
          agentId={agent?.agent_id || tool.toolCallId}
          size={30}
          status={
            status === "running" || status === "done" || status === "failed"
              ? status
              : undefined
          }
        />
        {status === "running" ? (
          <span className="lobe-subagent-card__running-dot" />
        ) : status === "done" ? (
          <span className="lobe-subagent-card__complete-badge">
            <IconCheck size={9} />
          </span>
        ) : null}
      </span>
      <span className="lobe-subagent-card__identity">
        <span className="lobe-subagent-card__meta">
          <strong>{nickname}</strong>
          <code>{subagentType}</code>
          {status === "failed" ? (
            <span className="lobe-subagent-card__exception">
              {statusLabel}
            </span>
          ) : null}
        </span>
        <small title={description}>{description}</small>
      </span>
    </>
  );
  const commonProps = {
    "aria-label": `${nickname}，${subagentType}，${description}，${statusLabel}`,
    "data-agent-id": agent?.agent_id || "",
    "data-agent-current": current ? "true" : "false",
    "data-agent-live": status === "running" ? "true" : "false",
    "data-agent-status": status,
  };

  return agent && onClick ? (
    <Button
      type="button"
      className="btn btn--ghost lobe-subagent-card"
      onClick={onClick}
      {...commonProps}
    >
      {content}
      <IconChevronRight
        className="lobe-subagent-card__chevron"
        size={14}
        aria-hidden="true"
      />
    </Button>
  ) : (
    <div
      className="lobe-subagent-card"
      role="status"
      {...commonProps}
    >
      {content}
    </div>
  );
}

/** 返回工具名称对应的紧凑动作文案。 */
function toolAction(tool: MessageToolSegment, locale: Locale): string {
  const category = timelineToolCategory(tool);
  const running = isToolSegmentRunning(tool);
  if (category === "ask-user")
    return locale === "zh" ? "询问用户" : "Ask user";
  if (category === "tool-search")
    return locale === "zh" ? "查找工具" : "Find tools";
  if (category === "skill-load")
    return locale === "zh" ? "加载 Skill" : "Load skill";
  if (category === "skill-search")
    return locale === "zh" ? "查找 Skill" : "Find skills";
  if (category === "web-search")
    return locale === "zh" ? "搜索网页" : "Search web";
  if (category === "web-fetch")
    return locale === "zh" ? "访问网页" : "Fetch web page";
  if (category === "tool-execute")
    return locale === "zh" ? "调用工具" : "Call tool";
  if (category === "wait-agent") {
    const outcome = waitAgentOutcome(tool);
    if (!running && outcome === "timeout")
      return locale === "zh" ? "等待超时" : "Wait timed out";
    if (!running && outcome === "agent_state_changed")
      return locale === "zh"
        ? "子 Agent 状态已变化"
        : "Subagent status changed";
    if (!running && outcome === "user_input")
      return locale === "zh"
        ? "等待因用户输入而结束"
        : "Wait ended on user input";
    if (!running && outcome === "turn_cancelled")
      return locale === "zh" ? "等待已取消" : "Wait cancelled";
    if (!running && outcome === "no_running_agents")
      return locale === "zh"
        ? "没有正在运行的子 Agent"
        : "No subagents running";
    return locale === "zh"
      ? running
        ? "等待"
        : "已等待"
      : running
        ? "Wait for"
        : "Waited for";
  }
  if (category === "folder") {
    return locale === "zh"
      ? running
        ? "浏览"
        : "已浏览"
      : running
        ? "Browse"
        : "Browsed";
  }
  if (category === "read") {
    return locale === "zh"
      ? running
        ? "读取"
        : "已读取"
      : "Read";
  }
  if (category === "search") {
    return locale === "zh"
      ? running
        ? "搜索"
        : "已搜索"
      : running
        ? "Search"
        : "Searched";
  }
  if (category === "edit") {
    return locale === "zh"
      ? running
        ? "编辑"
        : "已编辑"
      : running
        ? "Edit"
        : "Edited";
  }
  if (category === "command") {
    return locale === "zh"
      ? running
        ? "执行"
        : "已执行"
      : running
        ? "Execute"
        : "Executed";
  }
  return locale === "zh" ? "工具" : "Tool";
}

/** 返回与工具动作匹配的 Tabler 图标。 */
function ToolEvidenceIcon({ tool }: { tool: MessageToolSegment }) {
  switch (timelineToolCategory(tool)) {
    case "ask-user":
      return <IconUser size={17} />;
    case "tool-search":
    case "skill-search":
      return <IconSearch size={17} />;
    case "skill-load":
      return <IconPuzzle size={17} />;
    case "web-search":
      return <IconSearch size={17} />;
    case "web-fetch":
      return <IconExternalLink size={17} />;
    case "tool-execute":
      return <IconCode size={17} />;
    case "wait-agent":
      return <IconUser size={17} />;
    case "folder":
      return <IconFolder size={17} />;
    case "read":
      return <IconFileText size={17} />;
    case "search":
      return <IconSearch size={17} />;
    case "edit":
      return <IconEdit size={17} />;
    default:
      return <IconCode size={17} />;
  }
}

/** 将毫秒格式化为工具行使用的紧凑耗时。 */
function formatToolDuration(durationMs?: number | null): string {
  if (durationMs == null || !Number.isFinite(durationMs)) return "";
  return durationMs < 1000
    ? `${Math.round(durationMs)}ms`
    : `${(durationMs / 1000).toFixed(1)}s`;
}

/** 单条可展开的工具证据行。 */
export function TimelineToolRow({
  tool,
  locale = "en",
  onOpenResource,
  subagents = [],
  isLatestSubagentEvent = true,
}: {
  tool: MessageToolSegment;
  locale?: Locale;
  /** 点击已编辑文件时在右侧变更面板打开对应 Diff。 */
  onOpenResource?: (target: ResourceOpenTarget) => void;
  subagents?: AcpSubagentInfo[];
  /** 同一 child_thread_id 的最后一条 Agent 工具记录负责表达实时状态。 */
  isLatestSubagentEvent?: boolean;
}) {
  const failed = isToolSegmentFailed(tool);
  const running = isToolSegmentRunning(tool);
  const inputFields = parseToolInput(tool.input);
  const category = timelineToolCategory(tool);
  const planTool = isPlanTool(tool);
  const composerStateTool = planTool || isGoalTool(tool);
  const folderTool = category === "folder";
  const searchTool = category === "search";
  const readTool = category === "read" && !planTool;
  const editTool = category === "edit" && !planTool;
  const commandTool = category === "command";
  const askUserTool = category === "ask-user";
  const toolSearchTool = category === "tool-search";
  const skillLoadTool = category === "skill-load";
  const skillSearchTool = category === "skill-search";
  const webSearchTool = category === "web-search";
  const webFetchTool = category === "web-fetch";
  const executeExtraTool = category === "tool-execute";
  const waitAgentTool = category === "wait-agent";
  const waitTaskTitles = waitAgentTool
    ? waitAgentTaskTitles(tool, subagents)
    : [];
  const waitOutcome = waitAgentTool ? waitAgentOutcome(tool) : null;
  const resolvedPath = inputFields.path || tool.path;
  const readSummary = readPathLabel(
    resolvedPath || "",
    inputFields.offset,
    inputFields.limit,
  );
  const summary = folderTool
    ? (resolvedPath ? pathBasename(resolvedPath) : "") || toolSummary(tool)
    : searchTool
      ? inputFields.pattern || toolSummary(tool)
      : readTool
        ? readSummary || toolSummary(tool)
        : editTool
          ? (resolvedPath ? pathBasename(resolvedPath) : "") || toolSummary(tool)
          : commandTool
            ? inputFields.command || toolSummary(tool)
            : askUserTool
              ? inputFields.question || toolSummary(tool)
              : toolSearchTool || skillSearchTool
                ? inputFields.query || toolSummary(tool)
              : skillLoadTool
                ? inputFields.skillName || toolSummary(tool)
                : webSearchTool
                  ? inputFields.query || toolSummary(tool)
                  : webFetchTool
                    ? inputFields.url || toolSummary(tool)
                    : executeExtraTool
                      ? inputFields.toolName || toolSummary(tool)
                      : waitAgentTool
                        ? waitOutcome === "timeout"
                          ? waitTaskTitles.length
                            ? `「${waitTaskTitles.join(locale === "zh" ? "、" : ", ")}」${locale === "zh" ? "仍在运行" : " still running"}`
                            : locale === "zh"
                              ? "子 Agent 仍在运行"
                              : "subagents still running"
                          : running
                            ? waitTaskTitles.length
                              ? waitTaskTitles.join(locale === "zh" ? "、" : ", ")
                              : locale === "zh"
                                ? "子 Agent"
                                : "subagent"
                            : ""
                      : toolSummary(tool);
  const hasGenericDetail =
    !folderTool &&
    !searchTool &&
    !readTool &&
    !editTool &&
    !commandTool &&
    !askUserTool &&
    !toolSearchTool &&
    !skillLoadTool &&
    !skillSearchTool &&
    !webSearchTool &&
    !webFetchTool &&
    !executeExtraTool &&
    !waitAgentTool &&
    !planTool &&
    !!(tool.structuredResult || tool.output?.trim() || tool.detail?.trim());
  const hasDetail = failed || hasGenericDetail;
  const [open, setOpen] = useState(false);
  const pathTail =
    readTool || editTool
      ? resolvedPath
        ? pathBasename(resolvedPath)
        : ""
      : "";
  const duration = formatToolDuration(tool.durationMs);
  const action = toolAction(tool, locale);
  // 完成/失败状态只保留给辅助技术；工具行右侧不再重复显示终态文字。
  const statusLabel = failed
    ? locale === "zh"
      ? "失败"
      : "Failed"
    : running
      ? locale === "zh"
        ? "运行中"
        : "Running"
      : locale === "zh"
        ? "完成"
        : "Done";

  // Plan 与 Goal 由输入框上方的专用状态界面承载，不进入对话工具时间线。
  if (composerStateTool) return null;

  const subagent = subagentForTool(tool, subagents);
  if (subagent || classifyToolKind(tool.toolKind, tool.title) === "subagent") {
    return (
      <SubagentTimelineCard
        agent={subagent}
        tool={tool}
        locale={locale}
        current={isLatestSubagentEvent}
        failed={failed}
        onClick={
          subagent && onOpenResource
            ? () =>
                onOpenResource({ type: "subagent", agentId: subagent.agent_id })
            : undefined
        }
      />
    );
  }

  return (
    <div
      className={
        "lobe-timeline-tool" +
        (failed ? " is-error" : "") +
        (running ? " is-running" : "")
      }
      role="status"
      aria-label={`${action} ${summary} ${statusLabel}`}
      data-tool-id={tool.toolCallId}
      data-testid="timeline-tool"
    >
      <Button
        type="button"
        className="lobe-timeline-tool__row"
        aria-expanded={hasDetail ? open : undefined}
        disabled={!hasDetail && !(editTool && resolvedPath && onOpenResource)}
        onClick={() => {
          if (editTool && resolvedPath && onOpenResource) {
            onOpenResource({ type: "changes", path: resolvedPath });
            return;
          }
          if (hasDetail) setOpen((value) => !value);
        }}
      >
        <span className="lobe-timeline-tool__icon" aria-hidden>
          <ToolEvidenceIcon tool={tool} />
        </span>
        <span className="lobe-timeline-tool__action">
          {action}
        </span>
        <span
          className="lobe-timeline-tool__primary"
          title={resolvedPath || summary}
        >
          <span
            className={
              "lobe-timeline-tool__name" + (failed ? " is-error" : "")
            }
          >
            {pathTail || summary}
          </span>
          {pathTail && pathTail !== summary ? (
            <span className="lobe-timeline-tool__path">{summary}</span>
          ) : null}
        </span>
        {running || duration ? (
          <span
            className={
              "lobe-timeline-tool__meta" +
              (failed ? " is-error" : "") +
              (running ? " is-running" : "")
            }
          >
            {running ? (locale === "zh" ? "运行中" : "Running") : null}
            {duration ? <span>{duration}</span> : null}
          </span>
        ) : null}
        {hasDetail ? (
          <span
            className={
              "lobe-timeline-tool__chevron" + (open ? " is-open" : "")
            }
            aria-hidden
          >
            <IconChevronDown size={14} />
          </span>
        ) : null}
      </Button>
      {open && hasDetail ? (
        <div className="lobe-timeline-tool__detail">
          {tool.structuredResult &&
            (failed || (!readTool && !editTool && !commandTool)) ? (
            <StructuredToolResultView
              locale={locale}
              toolName={tool.toolKind || tool.title}
              result={tool.structuredResult as unknown as AcpStructuredToolResult}
            />
          ) : (
            <>
              {failed && (tool.output || tool.detail)?.trim() ? (
                <pre
                  className={
                    "lobe-timeline-tool__code" +
                    (failed ? " is-error" : "")
                  }
                >
                  {(tool.output || tool.detail)?.trim()}
                </pre>
              ) : null}
            </>
          )}
        </div>
      ) : null}
    </div>
  );
}

/** Map a tool_step ChatMessage to a MessageToolSegment for standalone rows. */
export function toolSegmentFromMessage(
  m: ChatMessage,
): MessageToolSegment | null {
  if (!isToolStepMessage(m)) return null;
  const tcid =
    (m.toolCallId || "").trim() ||
    (m.id.startsWith("tool-") ? m.id.slice(5) : m.id);
  if (!tcid) return null;
  const status = normalizeTaskStatus(
    m.toolStatus ||
      (m.content?.startsWith("tool_step|")
        ? parseToolStepContent(m.content)?.status
        : "") ||
      "",
    m.streaming,
  );
  return {
    kind: "tool",
    toolCallId: tcid,
    title: toolStepDisplayTitle(m) || tcid,
    toolKind: m.toolKind,
    status,
    detail: m.toolDetail,
    path: m.toolPath,
    streaming: !!m.streaming || status === "running",
    isError: !!m.isError || status === "failed",
  };
}

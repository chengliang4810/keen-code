import { Button } from "@appica/ui-react/button";
import { Badge } from "@appica/ui-react/badge";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@appica/ui-react/collapsible";
/**
 * Inline tool step on the assistant timeline (stream order).
 * Quiet red mark on failure; no bottom activity dump.
 */

import { useState } from "react";
import type { Locale } from "@/i18n";
import { t } from "@/i18n";
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
  compactToolFailureOutput,
  isGoalToolName,
  isPlanToolName,
  summarizeToolDisplay,
  toolCommandText,
} from "@/lib/toolDisplay";
import { extractToolInputFields, normalizeToolName } from "@/lib/toolInputFields";
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
import { TimelineImageGroup } from "./TimelineImageGroup";
import { isImageTool } from "@/lib/timelinePhases";
import type { ResourceOpenTarget } from "@/components/ResourceViewer";
import type { AcpSubagentInfo } from "@/lib/acp/store";
import { AgentAvatar } from "@/components/AgentAvatar";
import { agentNicknameLabel } from "@/lib/agentNicknames";
import {
  isToolSegmentFailed,
  isToolSegmentCancelled,
  isToolSegmentRunning,
} from "@/lib/toolSegmentStatus";

/** 在读取文件名后显示请求的行号范围。 */
function readPathLabel(path: string, offset?: number, limit?: number): string {
  const name = toolPathTail(path);
  if (!name || !limit) return name;
  const start = offset ?? 1;
  return `${name}:${start}\u2013${start + limit - 1}`;
}

/** 将路径转换成适合工具行显示的文件名。 */
function toolPathTail(path?: string): string {
  return path?.replace(/\\/g, "/").split("/").filter(Boolean).pop() || "";
}

type TimelineToolCategory =
  | "folder"
  | "read"
  | "search"
  | "edit"
  | "changes"
  | "command"
  | "agent"
  | "ask-user"
  | "tool-search"
  | "skill-load"
  | "plugin-command"
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
  const title = normalizeToolName(tool.title || "");
  const kind = normalizeToolName(tool.toolKind || "");

  const categoryFor = (value: string): TimelineToolCategory => {
    if (value === "askuser") {
      return "ask-user";
    }
    if (value === "searchextratools") {
      return "tool-search";
    }
    if (value === "skill") {
      return "skill-load";
    }
    // 模板读取不能被下方宽泛 command 匹配误判为终端执行。
    if (value === "plugincommand") {
      return "plugin-command";
    }
    if (value === "git" || value === "powershell" || value === "bash") {
      return "command";
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
    if (value === "wait_agent") {
      return "wait-agent";
    }
    if (
      value === "changes" ||
      value === "change" ||
      value === "changes_group" ||
      value === "change_group"
    ) {
      return "changes";
    }
    if (
      value === "agent" ||
      value === "spawn_agent" ||
      value === "send_message" ||
      value === "followup_task" ||
      value === "interrupt_agent"
    ) {
      return "agent";
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

  // 显式 renderer kind 是协议投影提供的稳定身份；仅在它不是 changes/agent
  // 等专用类别时才让具体标题覆盖通用 ACP kind（如 folder_operations/edit）。
  const kindCategory = categoryFor(kind);
  if (kindCategory === "changes" || kindCategory === "agent") {
    return kindCategory;
  }
  const titleCategory = categoryFor(title);
  return titleCategory === "other" ? categoryFor(kind) : titleCategory;
}

/**
 * 将 ACP 工具身份收敛到时间线 renderer 的稳定呈现类型。
 *
 * 这只是 UI 投影：原始 `toolKind`、标题、状态和文件快照仍由调用方保留，
 * renderer 只据已有证据选择图标、摘要和可展开行为，不虚构工具结果。
 */
export type TimelineToolRenderer =
  | "read"
  | "edit"
  | "execute"
  | "search"
  | "agent"
  | "changes"
  | "other";

export function timelineToolRenderer(
  tool: MessageToolSegment,
): TimelineToolRenderer {
  const category = timelineToolCategory(tool);
  if (category === "changes" || (tool.fileChanges?.length ?? 0) > 0) {
    return "changes";
  }
  switch (category) {
    case "read":
      return "read";
    case "edit":
      return "edit";
    case "command":
      return "execute";
    case "search":
      return "search";
    case "agent":
      return "agent";
    default:
      return "other";
  }
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

/** 运行中的 wait_agent 展示当前仍在执行的单层子 Agent。 */
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
  return [];
}

/** 读取 wait_agent 已有的结束原因，不从工具状态推测。 */
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
  ]
    .filter(Boolean)
    .join("\n");
  const byId = subagents.find((agent) => evidence.includes(agent.agent_id));
  if (byId) return byId;

  let requestedName = "";
  try {
    const input = JSON.parse(tool.input || "{}") as Record<string, unknown>;
    requestedName =
      typeof input.task_name === "string"
        ? input.task_name.trim()
        : "";
  } catch {
    /* 非 JSON 输入只能依赖稳定 Agent 标识。 */
  }
  const candidates = requestedName
    ? subagents.filter((agent) => agent.agent_name === requestedName)
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
    const description = [input.message].find(
      (value): value is string => typeof value === "string" && !!value.trim(),
    );
    return {
      description: description?.trim() || "",
      subagentType:
        typeof input.task_name === "string"
          ? input.task_name.trim()
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
    : isToolSegmentCancelled(tool)
      ? "interrupted"
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
        : status === "interrupted"
          ? locale === "zh"
            ? "已中断"
            : "Interrupted"
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
            status === "running" ||
            status === "done" ||
            status === "interrupted" ||
            status === "failed"
              ? status
              : undefined
          }
        />
        {status === "running" ? (
          <span className="lobe-subagent-card__running-dot" />
        ) : status === "done" ? (
          <Badge size="md" variant="success" aria-label={statusLabel}>
            <IconCheck size={9} />
          </Badge>
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
          ) : status === "interrupted" ? (
            <span className="lobe-subagent-card__status">
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
    <Button size="md"
      type="button"
      variant="ghost" className="lobe-subagent-card"
      onClick={onClick}
      {...commonProps}
      data-tool-renderer="agent"
      data-tool-kind={tool.toolKind || tool.title}
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
      data-tool-renderer="agent"
      data-tool-kind={tool.toolKind || tool.title}
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
  if (category === "plugin-command")
    return locale === "zh" ? "加载插件命令" : "Load plugin command";
  if (category === "web-search")
    return locale === "zh" ? "搜索网页" : "Search web";
  if (category === "web-fetch")
    return locale === "zh" ? "访问网页" : "Fetch web page";
  if (category === "tool-execute")
    return locale === "zh" ? "调用工具" : "Call tool";
  if (category === "wait-agent") {
    const outcome = waitAgentOutcome(tool);
    if (!running && outcome === "timed_out")
      return locale === "zh" ? "等待超时" : "Wait timed out";
    if (!running && outcome === "mailbox_activity")
      return locale === "zh"
        ? "Agent 邮箱已有新消息"
        : "Agent mailbox received activity";
    if (!running && outcome === "user_steer_activity")
      return locale === "zh"
        ? "收到用户追加消息"
        : "Received user steer";
    if (!running && outcome === "turn_ended")
      return locale === "zh"
        ? "等待期间 Turn 已结束"
        : "Turn ended while waiting";
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
  if (category === "changes") {
    return locale === "zh"
      ? running
        ? "修改"
        : "已修改"
      : running
        ? "Change"
        : "Changed";
  }
  if (category === "agent") {
    return locale === "zh"
      ? running
        ? "运行 Agent"
        : "已运行 Agent"
      : running
        ? "Run agent"
        : "Ran agent";
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
      return <IconSearch size={17} />;
    case "skill-load":
    case "plugin-command":
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
    case "changes":
      return <IconEdit size={17} />;
    case "agent":
      return <IconUser size={17} />;
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

/** 工具行展开后的详情体：展示规整后的原始正文。 */
export function TimelineToolDetailBody({
  tool,
  locale,
  failed,
  cancelled,
  rawAllowed,
}: {
  tool: MessageToolSegment;
  locale: Locale;
  failed: boolean;
  cancelled: boolean;
  /** 成功时是否允许回显原始正文；只有未分类工具为 true。 */
  rawAllowed: boolean;
}) {
  if (!(tool.output || tool.detail)?.trim()) return null;
  // 成功时只有未分类工具会走到这里；已知分类的成功正文不回显。
  if (!(failed || cancelled || rawAllowed)) return null;
  return (
    <pre
      className={"lobe-timeline-tool__code" + (failed ? " is-error" : "")}
    >
      {compactToolFailureOutput(tool.output || tool.detail, locale)}
    </pre>
  );
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
  /** 同一稳定 Agent 标识的最后一条协作工具记录负责表达实时状态。 */
  isLatestSubagentEvent?: boolean;
}) {
  const failed = isToolSegmentFailed(tool);
  const cancelled = isToolSegmentCancelled(tool);
  const running = isToolSegmentRunning(tool);
  const inputFields = extractToolInputFields(tool.input);
  const category = timelineToolCategory(tool);
  const renderer = timelineToolRenderer(tool);
  const planTool = isPlanTool(tool);
  const composerStateTool = planTool || isGoalTool(tool);
  const folderTool = category === "folder";
  const searchTool = category === "search";
  const readTool = category === "read" && !planTool;
  const changesTool = renderer === "changes";
  const editTool = (category === "edit" || changesTool) && !planTool;
  const commandTool = category === "command";
  const askUserTool = category === "ask-user";
  const toolSearchTool = category === "tool-search";
  const skillLoadTool = category === "skill-load";
  const pluginCommandTool = category === "plugin-command";
  const webSearchTool = category === "web-search";
  const webFetchTool = category === "web-fetch";
  const executeExtraTool = category === "tool-execute";
  const waitAgentTool = category === "wait-agent";
  const waitTaskTitles = waitAgentTool
    ? waitAgentTaskTitles(tool, subagents)
    : [];
  const waitOutcome = waitAgentTool ? waitAgentOutcome(tool) : null;
  const snapshotPath = tool.fileChanges?.find((change) => change.path.length > 0)?.path;
  const resolvedPath =
    snapshotPath || tool.path || inputFields.path;
  const readSummary = readPathLabel(
    resolvedPath || "",
    inputFields.offset,
    inputFields.limit,
  );
  const summary = folderTool
    ? toolPathTail(resolvedPath) || toolSummary(tool)
    : searchTool
      ? inputFields.pattern || toolSummary(tool)
      : readTool
        ? readSummary || toolSummary(tool)
        : editTool
          ? toolPathTail(resolvedPath) || toolSummary(tool)
          : commandTool
            ? toolCommandText({ kind: tool.toolKind, title: tool.title, input: tool.input }) || tool.title
            : askUserTool
              ? inputFields.question || toolSummary(tool)
              : toolSearchTool
                ? inputFields.query || toolSummary(tool)
              : skillLoadTool || pluginCommandTool
                ? inputFields.extensionName || tool.title || (skillLoadTool ? "Skill" : "PluginCommand")
                : webSearchTool
                  ? inputFields.query || toolSummary(tool)
                  : webFetchTool
                    ? inputFields.url || toolSummary(tool)
                    : executeExtraTool
                      ? inputFields.targetToolName || toolSummary(tool)
                      : waitAgentTool
                        ? waitOutcome === "timed_out"
                          ? ""
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
    !pluginCommandTool &&
    !webSearchTool &&
    !webFetchTool &&
    !executeExtraTool &&
    !waitAgentTool &&
    !planTool &&
    !!(tool.output?.trim() || tool.detail?.trim());
  // 可展开必须有实际内容：失败/取消但无正文时不渲染空详情区。
  const detailText = (tool.output || tool.detail)?.trim() || "";
  const hasDetail =
    (failed || cancelled || hasGenericDetail) && !!detailText;
  const [open, setOpen] = useState(false);
  const pathTail = readTool || editTool ? toolPathTail(resolvedPath) : "";
  const duration = formatToolDuration(tool.durationMs);
  const action = cancelled ? t(locale, "activity.cancelled") : toolAction(tool, locale);
  // 完成/失败状态只保留给辅助技术；工具行右侧不再重复显示终态文字。
  const statusLabel = cancelled
    ? t(locale, "activity.cancelled")
    : failed
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
  if (isImageTool(tool)) return <TimelineImageGroup tools={[tool]} locale={locale} />;

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

  const rowContent = (
    <>
      <span className="lobe-timeline-tool__icon" aria-hidden>
        <ToolEvidenceIcon tool={tool} />
      </span>
      <span
        className={
          "lobe-timeline-tool__action" +
          (running ? " animated-gradient-text" : "")
        }
      >
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
      {duration ? (
        <span
          className={"lobe-timeline-tool__meta" + (failed ? " is-error" : "")}
        >
          <span>{duration}</span>
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
    </>
  );
  const opensResource = Boolean(editTool && resolvedPath && onOpenResource);

  return (
    <Collapsible
      open={hasDetail ? open : false}
      onOpenChange={(nextOpen) => {
        if (hasDetail) setOpen(nextOpen);
      }}
      className={
        "lobe-timeline-tool" +
        ` is-renderer-${renderer}` +
        (failed ? " is-error" : "") +
        (running ? " is-running" : "")
      }
      role="status"
      aria-label={`${action} ${summary} ${statusLabel}`}
      data-tool-id={tool.toolCallId}
      data-tool-kind={tool.toolKind || tool.title}
      data-tool-renderer={renderer}
      data-tool-status={
        running
          ? "running"
          : cancelled
            ? "cancelled"
            : failed
              ? "failed"
              : "completed"
      }
      data-testid="timeline-tool"
    >
      {hasDetail || opensResource ? (
        <CollapsibleTrigger
          render={
            <Button
              type="button"
              variant="ghost"
              size="md"
              className="lobe-timeline-tool__row"
            />
          }
          onClick={(event) => {
            if (!opensResource || !resolvedPath || !onOpenResource) return;
            event.preventDefault();
            onOpenResource({
              type: "changes",
              path: resolvedPath,
              ...(tool.fileChanges !== undefined
                ? { fileChanges: tool.fileChanges }
                : {}),
            });
          }}
        >
          {rowContent}
        </CollapsibleTrigger>
      ) : (
        <div className="lobe-timeline-tool__row">
          {rowContent}
        </div>
      )}
      {hasDetail ? (
        <CollapsibleContent
          className="lobe-timeline-tool__detail-shell"
        >
          <div className="lobe-timeline-tool__detail">
          <TimelineToolDetailBody
            tool={tool}
            locale={locale}
            failed={failed}
            cancelled={cancelled}
            rawAllowed={hasGenericDetail}
          />
          </div>
        </CollapsibleContent>
      ) : null}
    </Collapsible>
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

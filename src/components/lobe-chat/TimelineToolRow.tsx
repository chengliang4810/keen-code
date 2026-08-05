/**
 * Inline tool step on the assistant timeline (stream order).
 * Quiet red mark on failure; no bottom activity dump.
 */

import { useMemo, useState } from "react";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import type { ChatMessage, MessageSegment, MessageToolSegment } from "@/lib/session";
import {
  isToolStepMessage,
  parseToolStepContent,
  toolStepDisplayTitle,
} from "@/lib/session";
import {
  isContextToolKind,
  isGoalToolName,
  isPlanToolName,
  summarizeToolDisplay,
} from "@/lib/toolDisplay";
import { normalizeTaskStatus } from "@/lib/sessionTasks";
import {
  IconChevronDown,
  IconCode,
  IconEdit,
  IconFileText,
  IconFolder,
  IconSearch,
} from "@/components/icons";
import type { AcpStructuredToolResult } from "@/lib/acp/types";
import { StructuredToolResultView } from "@/components/StructuredToolResultView";
import type { ResourceOpenTarget } from "@/components/ResourceViewer";

/** 工具输入中可用于界面展示的当前字段。 */
interface ToolInputFields {
  /** 文件工具的绝对或相对路径。 */
  path?: string;
  /** 文件搜索工具使用的匹配模式。 */
  pattern?: string;
  /** 命令工具的完整命令。 */
  command?: string;
}

/** 解析工具 JSON 参数，只提取当前界面明确支持的字段。 */
function parseToolInput(input?: string): ToolInputFields {
  if (!input?.trim()) return {};
  try {
    const value = JSON.parse(input) as Record<string, unknown>;
    const path = [value.file_path, value.folder_path, value.path]
      .find((item): item is string => typeof item === "string" && !!item.trim());
    const pattern =
      typeof value.pattern === "string" && value.pattern.trim()
        ? value.pattern
        : undefined;
    const command =
      typeof value.command === "string" && value.command.trim()
        ? value.command
        : undefined;
    return { path, pattern, command };
  } catch {
    return {};
  }
}

/** 将路径转换成适合工具行显示的文件名。 */
function toolPathTail(path?: string): string {
  return path?.replace(/\\/g, "/").split("/").filter(Boolean).pop() || "";
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

export function toolSegmentIsRunning(seg: MessageToolSegment): boolean {
  if (seg.streaming) return true;
  const s = (seg.status || "").toLowerCase();
  return s === "in_progress" || s === "pending" || s === "running" || s === "";
}

export function toolSegmentFailed(seg: MessageToolSegment): boolean {
  if (seg.isError) return true;
  const s = (seg.status || "").toLowerCase();
  return s === "failed" || s === "error" || s === "rejected" || s === "denied";
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

/** 返回工具名称对应的紧凑动作文案。 */
function toolAction(tool: MessageToolSegment, locale: Locale): string {
  const kind = (tool.toolKind || tool.title || "").toLowerCase();
  const running = toolSegmentIsRunning(tool);
  if (
    kind.includes("folder_operations") ||
    kind.includes("folder operations")
  ) {
    return locale === "zh"
      ? running
        ? "浏览"
        : "已浏览"
      : running
        ? "Browse"
        : "Browsed";
  }
  if (kind.includes("read")) {
    return locale === "zh"
      ? running
        ? "读取"
        : "已读取"
      : "Read";
  }
  if (
    kind.includes("grep") ||
    kind.includes("search") ||
    kind.includes("glob")
  ) {
    return locale === "zh"
      ? running
        ? "搜索"
        : "已搜索"
      : running
        ? "Search"
        : "Searched";
  }
  if (
    kind.includes("edit") ||
    kind.includes("write") ||
    kind.includes("patch")
  ) {
    return locale === "zh"
      ? running
        ? "编辑"
        : "已编辑"
      : running
        ? "Edit"
        : "Edited";
  }
  if (
    kind.includes("bash") ||
    kind.includes("shell") ||
    kind.includes("exec") ||
    kind.includes("terminal")
  ) {
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
  const kind = (tool.toolKind || tool.title || "").toLowerCase();
  if (
    kind.includes("folder_operations") ||
    kind.includes("folder operations")
  ) {
    return <IconFolder size={17} />;
  }
  if (kind.includes("read")) return <IconFileText size={17} />;
  if (
    kind.includes("grep") ||
    kind.includes("search") ||
    kind.includes("glob")
  ) {
    return <IconSearch size={17} />;
  }
  if (
    kind.includes("edit") ||
    kind.includes("write") ||
    kind.includes("patch")
  ) {
    return <IconEdit size={17} />;
  }
  if (
    kind.includes("bash") ||
    kind.includes("shell") ||
    kind.includes("exec") ||
    kind.includes("terminal")
  ) {
    return <IconCode size={17} />;
  }
  return <IconCode size={17} />;
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
}: {
  tool: MessageToolSegment;
  locale?: Locale;
  /** 点击已编辑文件时在右侧变更面板打开对应 Diff。 */
  onOpenResource?: (target: ResourceOpenTarget) => void;
}) {
  const failed = toolSegmentFailed(tool);
  const running = toolSegmentIsRunning(tool);
  const inputFields = parseToolInput(tool.input);
  const kind = (tool.toolKind || tool.title || "").toLowerCase();
  const planTool = isPlanTool(tool);
  const composerStateTool = planTool || isGoalTool(tool);
  const folderTool =
    kind.includes("folder_operations") || kind.includes("folder operations");
  const globTool = kind.includes("glob");
  const grepTool = kind.includes("grep");
  const readTool = kind.includes("read") && !planTool;
  const editTool =
    !planTool &&
    (kind.includes("edit") || kind.includes("write") || kind.includes("patch"));
  const commandTool =
    kind.includes("bash") ||
    kind.includes("shell") ||
    kind.includes("exec") ||
    kind.includes("terminal");
  const resolvedPath = inputFields.path || tool.path;
  const summary = folderTool
    ? toolPathTail(resolvedPath) || toolSummary(tool)
    : globTool || grepTool
      ? inputFields.pattern || toolSummary(tool)
      : readTool || editTool
        ? toolPathTail(resolvedPath) || toolSummary(tool)
        : commandTool
          ? inputFields.command || toolSummary(tool)
          : toolSummary(tool);
  const hasGenericDetail =
    !folderTool &&
    !globTool &&
    !grepTool &&
    !readTool &&
    !editTool &&
    !commandTool &&
    !planTool &&
    !!(tool.structuredResult || tool.output?.trim() || tool.detail?.trim());
  const hasDetail = failed || hasGenericDetail;
  const [open, setOpen] = useState(false);
  const pathTail = readTool || editTool ? toolPathTail(resolvedPath) : "";
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
      <button
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
      </button>
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

/** ≥3 consecutive context tools → collapsible group (CodePilot-style). */
export function TimelineContextGroup({
  tools,
  locale,
  onOpenResource,
}: {
  tools: MessageToolSegment[];
  locale: Locale;
  /** 从分组内工具行打开文件或变更。 */
  onOpenResource?: (target: ResourceOpenTarget) => void;
}) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [open, setOpen] = useState(() => tools.some(toolSegmentFailed));
  const running = tools.some(toolSegmentIsRunning);
  const hasErr = tools.some(toolSegmentFailed);

  return (
    <div
      className={
        "lobe-timeline-tool-group" +
        (hasErr ? " is-error" : "") +
        (running ? " is-running" : "")
      }
      data-testid="timeline-tool-group"
    >
      <button
        type="button"
        className="lobe-timeline-tool-group__trigger"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="lobe-timeline-tool-group__label">
          {running
            ? tr("turnActivity.gathering", { n: tools.length })
            : tr("turnActivity.gathered", { n: tools.length })}
        </span>
        <span
          className={
            "lobe-timeline-tool__status" +
            (hasErr ? " is-error" : "") +
            (running ? " is-running" : "")
          }
          aria-hidden
        />
      </button>
      {open ? (
        <div className="lobe-timeline-tool-group__list">
          {tools.map((t) => (
            <TimelineToolRow
              key={t.toolCallId}
              tool={t}
              locale={locale}
              onOpenResource={onOpenResource}
            />
          ))}
        </div>
      ) : null}
    </div>
  );
}

export type TimelineDisplayItem =
  | { type: "segment"; seg: MessageSegment; si: number }
  | { type: "tool-group"; tools: MessageToolSegment[]; startSi: number };

const CONTEXT_GROUP_MIN = 3;

/**
 * Walk message segments and collapse ≥3 consecutive context tools.
 * Thought / content / non-context tools stay as individual items.
 */
export function buildTimelineDisplayItems(
  segs: MessageSegment[],
  minContext = CONTEXT_GROUP_MIN,
): TimelineDisplayItem[] {
  const items: TimelineDisplayItem[] = [];
  let i = 0;
  while (i < segs.length) {
    const seg = segs[i]!;
    if (seg.kind !== "tool") {
      items.push({ type: "segment", seg, si: i });
      i += 1;
      continue;
    }
    // Peek consecutive context tools
    if (isContextToolKind(seg.toolKind, seg.title)) {
      const buf: MessageToolSegment[] = [seg];
      let j = i + 1;
      while (j < segs.length) {
        const n = segs[j]!;
        if (n.kind !== "tool") break;
        if (!isContextToolKind(n.toolKind, n.title)) break;
        buf.push(n);
        j += 1;
      }
      if (buf.length >= minContext) {
        items.push({ type: "tool-group", tools: buf, startSi: i });
        i = j;
        continue;
      }
    }
    items.push({ type: "segment", seg, si: i });
    i += 1;
  }
  return items;
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

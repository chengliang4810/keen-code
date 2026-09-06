/**
 * Lightweight tool display registry — shared by turn activity + tasks panel.
 * Summaries only; live mid-stream still prefers Host title via toolStepDisplayTitle.
 */

import { pathBasename } from "./filePath";

export type ToolDisplayKind =
  | "bash"
  | "read"
  | "edit"
  | "search"
  | "subagent"
  | "wait"
  | "web"
  | "meta"
  | "skill"
  | "plugin-command"
  | "ask"
  | "fallback";

export interface ToolDisplayInfo {
  kind: ToolDisplayKind;
  /** Short i18n-neutral label (English token; UI may map). */
  shortLabel: string;
  /** One-line summary for lists. */
  summary: string;
  /** True when this kind is "gathering context" (read/list/search). */
  isContext: boolean;
}

/** 工具组标题支持的界面语言。 */
type ToolDisplayLocale = "zh" | "zh-TW" | "en";

/** 工具输入中可用于工具组摘要的字段。 */
interface ToolSummaryInputFields {
  /** 文件或目录路径。 */
  path?: string;
  /** 文本搜索模式。 */
  pattern?: string;
  /** 终端执行命令。 */
  command?: string;
  /** 网页或元工具的查询。 */
  query?: string;
  /** WebFetch 目标网址。 */
  url?: string;
  /** ExecuteExtraTool 目标工具名。 */
  toolName?: string;
  /** Skill 或 PluginCommand 的当前 name 字段。 */
  extensionName?: string;
  /** AskUser 首个问题的 prompt。 */
  question?: string;
}

/** 生成工具组摘要所需的最小工具结构。 */
export interface ToolSummaryInput {
  /** 工具分类或协议名称。 */
  kind?: string | null;
  /** 工具界面标题。 */
  title?: string | null;
  /** 工具详情回退文本。 */
  detail?: string | null;
  /** 工具显式路径。 */
  path?: string | null;
  /** 工具 JSON 输入。 */
  input?: string | null;
  /** wait_agent 正在等待的子任务标题。 */
  waitTaskTitles?: string[];
  /** wait_agent 返回的结束原因。 */
  waitOutcome?: string | null;
}

/** 把工具名称标准化为当前界面分类使用的稳定键。 */
function normalizedToolName(value: string | null | undefined): string {
  return (value || "").trim().toLowerCase().replace(/[\s./-]+/g, "_");
}

/** 从当前命令工具参数生成展示文本；Git 参数只作可读引用，不作为 Shell 脚本执行。 */
export function toolCommandText(tool: ToolSummaryInput): string | undefined {
  if (classifyToolKind(tool.kind, tool.title) !== "bash" || !tool.input) return undefined;
  try {
    const value: unknown = JSON.parse(tool.input);
    if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
    const fields = value as Record<string, unknown>;
    if ([tool.kind, tool.title].map(normalizedToolName).includes("git")) {
      if (!Array.isArray(fields.args) || fields.args.length === 0 ||
        !fields.args.every((arg): arg is string => typeof arg === "string")) return undefined;
      return `git ${fields.args.map(arg => /^[\w./:@=+-]+$/.test(arg) ? arg : JSON.stringify(arg)).join(" ")}`;
    }
    return typeof fields.command === "string" && fields.command.trim()
      ? fields.command.trim()
      : undefined;
  } catch {
    return undefined;
  }
}

/** 判断工具名称是否属于 Plan/Todo 状态工具。 */
export function isPlanToolName(
  kind: string | null | undefined,
  title?: string | null,
): boolean {
  return [kind, title].some((value) => {
    const name = normalizedToolName(value);
    return (
      name === "todo" ||
      name === "todowrite" ||
      name === "todo_write" ||
      name === "plan" ||
      name === "update_plan"
    );
  });
}

/** 判断工具名称是否属于持久 Goal 状态工具。 */
export function isGoalToolName(
  kind: string | null | undefined,
  title?: string | null,
): boolean {
  return [kind, title].some((value) => {
    const name = normalizedToolName(value);
    return (
      name === "goal" ||
      name === "create_goal" ||
      name === "get_goal" ||
      name === "update_goal" ||
      name === "goal_upsert" ||
      name === "goal_transition" ||
      name === "goal_clear"
    );
  });
}

function clip(s: string, max = 56): string {
  const t = s.trim();
  if (t.length <= max) return t;
  return `${t.slice(0, max - 1).trimEnd()}…`;
}

/** 解析工具输入，只提取可安全展示的路径、模式和命令。 */
function parseToolSummaryInput(input?: string | null): ToolSummaryInputFields {
  if (!input?.trim()) return {};
  try {
    const value = JSON.parse(input) as Record<string, unknown>;
    const path = [value.file_path, value.folder_path, value.path].find(
      (item): item is string => typeof item === "string" && !!item.trim(),
    );
    const pattern =
      typeof value.pattern === "string" && value.pattern.trim()
        ? value.pattern
        : undefined;
    const command = [value.command, value.cmd].find(
      (item): item is string => typeof item === "string" && !!item.trim(),
    );
    const query =
      typeof value.query === "string" && value.query.trim()
        ? value.query
        : undefined;
    const url =
      typeof value.url === "string" && value.url.trim() ? value.url : undefined;
    const toolName =
      typeof value.tool_name === "string" && value.tool_name.trim()
        ? value.tool_name
        : undefined;
    const extensionName =
      typeof value.name === "string" && value.name.trim()
        ? value.name
        : undefined;
    const questions = Array.isArray(value.questions) ? value.questions : [];
    const question = questions
      .map((item) =>
        item && typeof item === "object"
          ? (item as Record<string, unknown>).prompt
          : undefined,
      )
      .find(
        (item): item is string =>
          typeof item === "string" && !!item.trim(),
      );
    return {
      path,
      pattern,
      command,
      query,
      url,
      toolName,
      extensionName,
      question,
    };
  } catch {
    return {};
  }
}

/** 返回工具分类对应的进行中动作。 */
function runningToolAction(
  kind: ToolDisplayKind,
  locale: ToolDisplayLocale,
): string {
  if (locale === "en") {
    switch (kind) {
      case "read":
        return "Reading";
      case "edit":
        return "Editing";
      case "search":
        return "Searching";
      case "bash":
        return "Running";
      case "subagent":
        return "Running agent";
      case "web":
        return "Using web";
      case "meta":
        return "Calling tool";
      case "skill":
        return "Using skill";
      case "plugin-command":
        return "Loading plugin command";
      case "ask":
        return "Asking user";
      default:
        return "Running tool";
    }
  }
  if (locale === "zh-TW") {
    switch (kind) {
      case "read":
        return "正在讀取";
      case "edit":
        return "正在編輯";
      case "search":
        return "正在搜尋";
      case "bash":
        return "正在執行";
      case "subagent":
        return "正在執行子 Agent";
      case "web":
        return "正在使用網頁";
      case "meta":
        return "正在呼叫工具";
      case "skill":
        return "正在使用 Skill";
      case "plugin-command":
        return "正在載入外掛命令";
      case "ask":
        return "正在詢問使用者";
      default:
        return "正在呼叫工具";
    }
  }
  switch (kind) {
    case "read":
      return "正在读取";
    case "edit":
      return "正在编辑";
    case "search":
      return "正在搜索";
    case "bash":
      return "正在运行";
    case "subagent":
      return "正在运行子 Agent";
    case "wait":
      return "正在等待子 Agent";
    case "web":
      return "正在使用网页";
    case "meta":
      return "正在调用工具";
    case "skill":
      return "正在使用 Skill";
    case "plugin-command":
      return "正在加载插件命令";
    case "ask":
      return "正在询问用户";
    default:
      return "正在调用工具";
  }
}

/** 返回工具分类对应的历史完成动作。 */
function completedToolAction(
  kind: ToolDisplayKind,
  locale: ToolDisplayLocale,
): string {
  if (locale === "en") {
    switch (kind) {
      case "read":
        return "read files";
      case "edit":
        return "edited files";
      case "search":
        return "searched code";
      case "bash":
        return "ran commands";
      case "subagent":
        return "ran subagents";
      case "wait":
        return "waited for subagents";
      case "web":
        return "used the web";
      case "meta":
        return "called tools";
      case "skill":
        return "used skills";
      case "plugin-command":
        return "loaded plugin commands";
      case "ask":
        return "asked the user";
      default:
        return "used tools";
    }
  }
  if (locale === "zh-TW") {
    switch (kind) {
      case "read":
        return "讀取了檔案";
      case "edit":
        return "編輯了檔案";
      case "search":
        return "搜尋了程式碼";
      case "bash":
        return "執行了命令";
      case "subagent":
        return "執行了子 Agent";
      case "wait":
        return "等待了子 Agent";
      case "web":
        return "使用了網頁";
      case "meta":
        return "呼叫了工具";
      case "skill":
        return "使用了 Skill";
      case "plugin-command":
        return "載入了外掛命令";
      case "ask":
        return "詢問了使用者";
      default:
        return "呼叫了工具";
    }
  }
  switch (kind) {
    case "read":
      return "读取了文件";
    case "edit":
      return "编辑了文件";
    case "search":
      return "搜索了代码";
    case "bash":
      return "运行了命令";
    case "subagent":
      return "运行了子 Agent";
    case "wait":
      return "等待了子 Agent";
    case "web":
      return "使用了网页";
    case "meta":
      return "调用了工具";
    case "skill":
      return "使用了 Skill";
    case "plugin-command":
      return "加载了插件命令";
    case "ask":
      return "询问了用户";
    default:
      return "调用了工具";
  }
}

/** 返回进行中工具组最后一个正在调用工具的自然语言描述。 */
export function summarizeRunningTool(
  tool: ToolSummaryInput,
  locale: ToolDisplayLocale,
): string {
  const toolNames = [tool.kind, tool.title].map(normalizedToolName);
  if (toolNames.includes("wait_agent")) {
    const titles = (tool.waitTaskTitles || []).filter(Boolean);
    if (locale === "en") {
      if (titles.length === 1) return `Waiting for “${clip(titles[0]!, 72)}”…`;
      if (titles.length === 2) {
        return `Waiting for “${clip(titles[0]!, 48)}” and “${clip(titles[1]!, 48)}”…`;
      }
      if (titles.length > 2) {
        return `Waiting for “${clip(titles[0]!, 48)}” and ${titles.length - 1} other tasks…`;
      }
      return "Waiting for subtask…";
    }
    if (locale === "zh-TW") {
      if (titles.length === 1) return `正在等待「${clip(titles[0]!, 72)}」完成…`;
      if (titles.length === 2) {
        return `正在等待「${clip(titles[0]!, 48)}」和「${clip(titles[1]!, 48)}」完成…`;
      }
      if (titles.length > 2) {
        return `正在等待「${clip(titles[0]!, 48)}」等 ${titles.length} 個子任務完成…`;
      }
      return "正在等待子任務完成…";
    }
    if (titles.length === 1) return `正在等待「${clip(titles[0]!, 72)}」完成…`;
    if (titles.length === 2) {
      return `正在等待「${clip(titles[0]!, 48)}」和「${clip(titles[1]!, 48)}」完成…`;
    }
    if (titles.length > 2) {
      return `正在等待「${clip(titles[0]!, 48)}」等 ${titles.length} 个子任务完成…`;
    }
    return "正在等待子任务完成…";
  }
  const kind = classifyToolKind(tool.kind, tool.title);
  const fields = parseToolSummaryInput(tool.input);
  const explicitPath = fields.path || tool.path || "";
  const target =
    kind === "bash"
      ? toolCommandText(tool)
      : kind === "web"
        ? fields.query || fields.url
        : kind === "meta"
          ? fields.query || fields.toolName
          : kind === "skill" || kind === "plugin-command"
            ? fields.extensionName
            : kind === "ask"
              ? fields.question
              : kind === "search"
                ? fields.pattern ||
                  (explicitPath ? pathBasename(explicitPath) : undefined)
                : explicitPath
                  ? pathBasename(explicitPath)
                  : summarizeToolDisplay(tool).summary;
  const action = runningToolAction(kind, locale);
  return target ? `${action} ${clip(target, 96)}` : action;
}

/** 按首次出现顺序汇总历史工具组实际调用过的工具类型。 */
export function summarizeCompletedTools(
  tools: ToolSummaryInput[],
  locale: ToolDisplayLocale,
): string {
  const kinds: ToolDisplayKind[] = [];
  const actions: string[] = [];
  for (const tool of tools) {
    const kind = classifyToolKind(tool.kind, tool.title);
    if (kinds.includes(kind)) continue;
    kinds.push(kind);
    if (kind === "wait") {
      const titles = (tool.waitTaskTitles || []).filter(Boolean);
      if (tool.waitOutcome === "timed_out") {
        const target = titles.length
          ? `「${titles.slice(0, 2).map((title) => clip(title, 48)).join("、")}」`
          : locale === "en"
            ? "subagents"
            : "子 Agent";
        actions.push(
          locale === "en"
            ? `wait timed out for ${target}`
            : `等待 ${target} 超时`,
        );
      } else if (tool.waitOutcome === "mailbox_activity") {
        actions.push(
          locale === "en" ? "agent mailbox received activity" : "Agent 邮箱已有新消息",
        );
      } else if (tool.waitOutcome === "user_steer_activity") {
        actions.push(
          locale === "en"
            ? "received user steer"
            : "收到用户追加消息",
        );
      } else if (tool.waitOutcome === "turn_ended") {
        actions.push(
          locale === "en" ? "turn ended while waiting" : "等待期间 Turn 已结束",
        );
      } else {
        actions.push(completedToolAction(kind, locale));
      }
      continue;
    }
    actions.push(completedToolAction(kind, locale));
  }
  if (locale === "en") return actions.join(", ");
  return actions.join("、");
}

/** Classify a raw tool kind / title into a display bucket. */
export function classifyToolKind(
  kind: string | null | undefined,
  title?: string | null,
): ToolDisplayKind {
  const k = normalizedToolName(kind);
  const t = normalizedToolName(title);
  const names = [k, t];
  if (
    names.some(
      (name) =>
        name === "websearch" ||
        name === "web_search" ||
        name === "webfetch" ||
        name === "web_fetch",
    )
  ) {
    return "web";
  }
  if (
    names.some(
      (name) =>
        name === "toolsearch" ||
        name === "executeextratool" ||
        name === "execute_extra_tool",
    )
  ) {
    return "meta";
  }
  if (names.includes("skill")) {
    return "skill";
  }
  if (names.includes("plugincommand")) {
    return "plugin-command";
  }
  if (names.includes("askuser")) {
    return "ask";
  }
  if (names.some(name => ["bash", "powershell", "git", "execute"].includes(name))) {
    return "bash";
  }
  if (
    names.some((name) =>
      [
        "spawn_agent",
        "send_message",
        "followup_task",
        "interrupt_agent",
      ].includes(name),
    )
  ) {
    return "subagent";
  }
  if (names.some((name) => name === "wait_agent")) {
    return "wait";
  }
  if (
    k === "write" ||
    k === "edit" ||
    k === "folder_operations" ||
    t === "write" ||
    t === "edit"
  ) {
    return "edit";
  }
  if (k === "grep" || k === "glob" || k === "search" || t === "grep" || t === "glob") {
    return "search";
  }
  if (k === "read" || t === "read") {
    return "read";
  }
  return "fallback";
}

export function isContextToolKind(
  kind: string | null | undefined,
  title?: string | null,
): boolean {
  const c = classifyToolKind(kind, title);
  return c === "read" || c === "search";
}

export function toolShortLabel(kind: ToolDisplayKind): string {
  switch (kind) {
    case "bash":
      return "Shell";
    case "read":
      return "Read";
    case "edit":
      return "Edit";
    case "search":
      return "Search";
    case "subagent":
      return "Agent";
    case "wait":
      return "Wait";
    case "web":
      return "Web";
    case "meta":
      return "Tool";
    case "skill":
      return "Skill";
    case "plugin-command":
      return "PluginCommand";
    case "ask":
      return "Question";
    default:
      return "Tool";
  }
}

/**
 * Human summary for a tool row.
 * Prefer path basename / detail snippet over bare kind.
 */
export function summarizeToolDisplay(input: {
  kind?: string | null;
  title?: string | null;
  detail?: string | null;
  path?: string | null;
}): ToolDisplayInfo {
  const bucket = classifyToolKind(input.kind, input.title);
  const path = (input.path || "").trim();
  const detail = (input.detail || "").trim();
  const title = (input.title || "").trim();
  let summary = "";
  if (path) {
    summary = pathBasename(path);
    if (bucket === "bash" && detail) {
      summary = clip(detail.split("\n")[0] || detail);
    }
  } else if (detail) {
    summary = clip(detail.split("\n")[0] || detail);
  } else if (title && !/^tool$/i.test(title)) {
    summary = clip(title);
  } else if (input.kind) {
    summary = clip(input.kind.replace(/[_./]+/g, " "));
  } else {
    summary = toolShortLabel(bucket);
  }
  return {
    kind: bucket,
    shortLabel: toolShortLabel(bucket),
    summary,
    isContext: bucket === "read" || bucket === "search",
  };
}

/** Last N non-empty lines of tool detail (expanded activity). */
export function toolDetailTail(
  detail: string | null | undefined,
  maxLines = 8,
): string {
  if (!detail?.trim()) return "";
  const lines = detail.replace(/\r\n/g, "\n").split("\n");
  const kept = lines.filter((l, i) => l.trim() || i === lines.length - 1);
  if (kept.length <= maxLines) return kept.join("\n");
  return kept.slice(-maxLines).join("\n");
}

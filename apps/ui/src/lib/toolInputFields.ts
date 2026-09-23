/**
 * Shared extractor for display-oriented tool input fields.
 * Single source consumed by tool summaries (toolDisplay) and timeline rows.
 */

/** 工具输入中可用于界面展示的字段；未识别或非法的字段一律缺省。 */
export interface ToolInputFields {
  /** 文件或目录路径（file_path / folder_path / path 首个非空字符串）。 */
  path?: string;
  /** 文本搜索模式。 */
  pattern?: string;
  /** 终端执行命令（command / cmd 首个非空字符串）。 */
  command?: string;
  /** AskUser 首个问题的 prompt。 */
  question?: string;
  /** 工具或 Skill 搜索关键词。 */
  query?: string;
  /** Skill 或 PluginCommand 请求加载的当前 name。 */
  extensionName?: string;
  /** WebFetch 请求访问的网址。 */
  url?: string;
  /** ExecuteExtraTool 代理调用的真实工具名（tool_name）。 */
  targetToolName?: string;
  /** Read 工具请求的 1-based 起始行。 */
  offset?: number;
  /** Read 工具请求的行数。 */
  limit?: number;
}

/** 从字符串或已解析值中取出可校验的 JSON 对象；其余输入按缺省处理。 */
function asRecord(input: unknown): Record<string, unknown> | null {
  if (typeof input === "string") {
    if (!input.trim()) return null;
    try {
      const value: unknown = JSON.parse(input);
      return value && typeof value === "object" && !Array.isArray(value)
        ? (value as Record<string, unknown>)
        : null;
    } catch {
      return null;
    }
  }
  return input && typeof input === "object" && !Array.isArray(input)
    ? (input as Record<string, unknown>)
    : null;
}

/** 返回首个非空字符串候选；保留原文，不做 trim。 */
function firstNonEmptyString(values: readonly unknown[]): string | undefined {
  return values.find(
    (item): item is string => typeof item === "string" && !!item.trim(),
  );
}

/** 非空字符串字段：空串与纯空白视为缺省，命中时保留原文。 */
function nonEmptyString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

/** 正整数计数字段：非正整数视为缺省。 */
function positiveInteger(value: unknown): number | undefined {
  return Number.isInteger(value) && (value as number) > 0
    ? (value as number)
    : undefined;
}

/** AskUser questions 中首个非空 prompt。 */
function firstQuestionPrompt(value: unknown): string | undefined {
  const questions = Array.isArray(value) ? value : [];
  return questions
    .map((item) =>
      item && typeof item === "object"
        ? (item as Record<string, unknown>).prompt
        : undefined,
    )
    .find((item): item is string => typeof item === "string" && !!item.trim());
}

/** 解析工具 JSON 输入，只提取当前界面明确支持的可展示字段。 */
export function extractToolInputFields(input: unknown): ToolInputFields {
  const value = asRecord(input);
  if (!value) return {};
  return {
    path: firstNonEmptyString([value.file_path, value.folder_path, value.path]),
    pattern: nonEmptyString(value.pattern),
    command: firstNonEmptyString([value.command, value.cmd]),
    question: firstQuestionPrompt(value.questions),
    query: nonEmptyString(value.query),
    extensionName: nonEmptyString(value.name),
    url: nonEmptyString(value.url),
    targetToolName: nonEmptyString(value.tool_name),
    offset: positiveInteger(value.offset),
    limit: positiveInteger(value.limit),
  };
}

/** 把工具名称标准化为当前界面分类使用的稳定键。 */
export function normalizeToolName(name: string): string {
  return (name || "").trim().toLowerCase().replace(/[\s./-]+/g, "_");
}

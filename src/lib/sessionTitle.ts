/** 自动生成标题允许保留的最大 Unicode 字符数。 */
export const SESSION_TITLE_MAX_CHARS = 36;

/** 可用于提取首条用户文本的最小消息结构。 */
export interface SessionTitleMessage {
  /** 消息角色；只有 `user` 会作为标题来源。 */
  role: string;
  /** 消息正文；非字符串内容会被忽略。 */
  content?: string | null;
}

/** 首个成功回合中供标题模型概括的用户与 Assistant 正文。 */
export interface SuccessfulTurnTitleInput {
  /** 已移除 Goal、Skill 与 Markdown 控制标记的用户正文。 */
  userMessage: string;
  /** 首条有效 Assistant 回复正文。 */
  assistantMessage: string;
}

/** 自动标题生成前的当前标题状态。 */
export interface AutomaticSessionTitleEligibility {
  /** 当前持久化或界面投影标题。 */
  currentTitle?: string | null;
  /** 当前标题来源；手动标题绝不允许覆盖，消息前缀标题允许自动短标题覆盖。 */
  titleSource?: "manual" | "automatic" | "message-prefix" | string | null;
  /** 当前语言包提供的额外占位标题。 */
  localizedPlaceholders?: readonly string[];
}

/** 默认视为尚未命名的中英文标题。 */
export const DEFAULT_PLACEHOLDER_SESSION_TITLES = [
  "New chat",
  "New task",
  "New conversation",
  "Untitled",
  "新会话",
  "新对话",
  "新任务",
  "未命名",
  "无标题",
] as const;

/** KeenCode 草稿中持久化的 Skill 控制标记。 */
const STORED_SKILL_TOKEN_RE = /\[\[skill:[a-zA-Z0-9_.:-]+\]\]/g;

/** 发送给 Agent 时位于首行的 Skill 控制标记。 */
const AGENT_SKILL_LINE_RE =
  /^(?:\/[a-zA-Z0-9_.:-]+)(?:[ \t]+\/[a-zA-Z0-9_.:-]+)*[ \t]*(?:\r?\n|$)/;

/** 标题模型可能返回的英文或中文字段前缀。 */
const TITLE_PREFIX_RE = /^(?:(?:session[ \t]+)?title|标题)[ \t]*[:：-][ \t]*/i;

/** 标题两端可移除的成对引号与 Markdown 代码标记。 */
const TITLE_WRAPPER_PAIRS: ReadonlyArray<readonly [string, string]> = [
  ['"', '"'],
  ["'", "'"],
  ["`", "`"],
  ["“", "”"],
  ["‘", "’"],
  ["「", "」"],
  ["『", "』"],
  ["《", "》"],
];

/** 按 Unicode 码点截断，避免切断代理对。 */
function truncateUnicode(value: string, maxChars: number): string {
  if (maxChars <= 0) return "";
  return Array.from(value).slice(0, maxChars).join("");
}

/** 移除一层或多层包围整个标题的引号、书名号或反引号。 */
function stripTitleWrappers(value: string): string {
  let next = value.trim();
  let changed = true;
  while (changed && next.length >= 2) {
    changed = false;
    for (const [opening, closing] of TITLE_WRAPPER_PAIRS) {
      if (!next.startsWith(opening) || !next.endsWith(closing)) continue;
      next = next.slice(opening.length, -closing.length).trim();
      changed = true;
      break;
    }
  }
  return next;
}

/** 解码标题中常见的 Markdown/HTML 实体，不依赖浏览器 DOM。 */
function decodeCommonEntities(value: string): string {
  const namedEntities: Readonly<Record<string, string>> = {
    amp: "&",
    apos: "'",
    gt: ">",
    hellip: "…",
    lt: "<",
    nbsp: " ",
    quot: '"',
  };
  return value
    .replace(
      /&(#(?:x[0-9a-f]+|\d+)|[a-z]+);/gi,
      (entity, entityBody: string) => {
        if (entityBody.startsWith("#x") || entityBody.startsWith("#X")) {
          const codePoint = Number.parseInt(entityBody.slice(2), 16);
          return Number.isFinite(codePoint) &&
            codePoint >= 0 &&
            codePoint <= 0x10ffff
            ? String.fromCodePoint(codePoint)
            : entity;
        }
        if (entityBody.startsWith("#")) {
          const codePoint = Number.parseInt(entityBody.slice(1), 10);
          return Number.isFinite(codePoint) &&
            codePoint >= 0 &&
            codePoint <= 0x10ffff
            ? String.fromCodePoint(codePoint)
            : entity;
        }
        return namedEntities[entityBody.toLowerCase()] ?? entity;
      },
    )
    .replace(/\u00a0/g, " ");
}

/**
 * 将用户 Markdown 转为适合标题使用的可读纯文本。
 * 保留链接文案、图片替代文本和代码内容，仅移除展示语法。
 */
export function markdownToReadableTitleText(markdown: string): string {
  return decodeCommonEntities(markdown)
    .replace(/\r\n?/g, "\n")
    .replace(/^ {0,3}\[[^\]\n]+\]:\s+\S+.*$/gm, " ")
    .replace(/!\[([^\]]*)\]\([^)\n]*\)/g, "$1")
    .replace(/\[([^\]]+)\]\([^)\n]*\)/g, "$1")
    .replace(/\[([^\]]+)\]\[[^\]]*\]/g, "$1")
    .replace(/<((?:https?:\/\/|mailto:)[^>\s]+)>/gi, "$1")
    .replace(/<[^>\n]+>/g, " ")
    .replace(/^ {0,3}```[^\n]*$/gm, " ")
    .replace(/^ {0,3}~~~[^\n]*$/gm, " ")
    .replace(/^ {0,3}#{1,6}[ \t]+/gm, "")
    .replace(/^ {0,3}>[ \t]?/gm, "")
    .replace(/^ {0,3}(?:[-+*]|\d+[.)])[ \t]+(?:\[[ xX]\][ \t]+)?/gm, "")
    .replace(/(`+)([\s\S]*?)\1/g, "$2")
    .replace(/(\*\*|__|~~)(.*?)\1/g, "$2")
    .replace(/(^|[^\w])[*_]([^*_\n]+)[*_](?=$|[^\w])/g, "$1$2")
    .replace(/\\([\\`*_[\]{}()#+\-.!>])/g, "$1")
    .replace(/[ \t]*\|[ \t]*/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

/**
 * 从一条用户消息中移除 Skill 控制包装，并返回可显示纯文本。
 * 支持草稿态 `[[skill:name]]` 与发送态 `/skill` 首行。
 */
export function extractDisplayTextFromUserMessage(
  content: string | null | undefined,
): string {
  if (!content?.trim()) return "";

  let visible = content.replace(/\r\n?/g, "\n").trim();
  visible = visible.replace(STORED_SKILL_TOKEN_RE, " ");

  const firstLine = visible.match(/^[^\n]*/)?.[0]?.trim() ?? "";
  if (firstLine && AGENT_SKILL_LINE_RE.test(visible)) {
    visible = visible.replace(AGENT_SKILL_LINE_RE, "");
  }

  return markdownToReadableTitleText(visible);
}

/** 从消息序列中提取首条具有可显示正文的用户消息。 */
export function extractFirstUserMessageText(
  messages: readonly SessionTitleMessage[],
): string {
  for (const message of messages) {
    if (message.role.toLowerCase() !== "user") continue;
    const visible = extractDisplayTextFromUserMessage(message.content);
    if (visible) return visible;
  }
  return "";
}

/**
 * 提取首个已经收到 Assistant 正文回复的用户消息。
 * 如果某条用户消息后先出现下一条用户消息，则该轮视为未成功完成。
 */
export function extractFirstSuccessfulTurnUserMessageText(
  messages: readonly SessionTitleMessage[],
): string {
  return extractFirstSuccessfulTurnTitleInput(messages)?.userMessage ?? "";
}

/**
 * 提取首个已收到 Assistant 正文回复的完整回合，供独立标题模型调用。
 * 如果某条用户消息后先出现下一条用户消息，则该轮视为未成功完成。
 */
export function extractFirstSuccessfulTurnTitleInput(
  messages: readonly SessionTitleMessage[],
): SuccessfulTurnTitleInput | null {
  for (let index = 0; index < messages.length; index += 1) {
    const message = messages[index];
    if (message.role.toLowerCase() !== "user") continue;
    const visible = extractDisplayTextFromUserMessage(message.content);
    if (!visible) continue;

    for (let nextIndex = index + 1; nextIndex < messages.length; nextIndex += 1) {
      const nextMessage = messages[nextIndex];
      const nextRole = nextMessage.role.toLowerCase();
      if (nextRole === "user") break;
      if (nextRole === "assistant" && nextMessage.content?.trim()) {
        return {
          userMessage: visible,
          assistantMessage: nextMessage.content.trim(),
        };
      }
    }
  }
  return null;
}

/** 从消息序列构建默认标题：首条用户消息截取开头。 */
export function buildSessionTitleFromFirstMessage(
  messages: readonly SessionTitleMessage[],
): string {
  return truncateUnicode(
    extractFirstUserMessageText(messages),
    SESSION_TITLE_MAX_CHARS,
  ).trim();
}

/**
 * 净化自动生成的标题候选。
 * 返回空字符串表示 Agent 候选不可用，调用方不应使用用户原文回退。
 */
export function sanitizeGeneratedSessionTitle(
  candidate: string | null | undefined,
): string {
  if (!candidate?.trim()) return "";

  let title = markdownToReadableTitleText(candidate);
  title = stripTitleWrappers(title);
  title = title.replace(TITLE_PREFIX_RE, "").trim();
  title = stripTitleWrappers(title);
  title = title.replace(/[ \t]*[,.!?;:，。！？；：、…]+$/u, "").trim();
  title = stripTitleWrappers(title);
  title = truncateUnicode(title, SESSION_TITLE_MAX_CHARS).trim();
  title = title.replace(/[ \t]*[,.!?;:，。！？；：、…]+$/u, "").trim();
  return stripTitleWrappers(title);
}

/**
 * 判断标题是否仍是新任务占位值。
 * 调用方可传入当前语言包中的额外占位文案。
 */
export function isPlaceholderSessionTitle(
  title: string | null | undefined,
  localizedPlaceholders: readonly string[] = [],
): boolean {
  const normalized = title?.replace(/\s+/g, " ").trim().toLocaleLowerCase();
  if (!normalized) return true;

  return [...DEFAULT_PLACEHOLDER_SESSION_TITLES, ...localizedPlaceholders].some(
    (placeholder) =>
      placeholder.replace(/\s+/g, " ").trim().toLocaleLowerCase() ===
      normalized,
  );
}

/** 判断当前标题是否允许由独立标题模型替换。 */
export function canGenerateAutomaticSessionTitle({
  currentTitle,
  titleSource,
  localizedPlaceholders = [],
}: AutomaticSessionTitleEligibility): boolean {
  if (titleSource === "manual") return false;
  // 消息前缀标题只是临时默认值，与占位符同权，允许自动短标题覆盖。
  if (titleSource === "message-prefix") return true;
  return isPlaceholderSessionTitle(currentTitle, localizedPlaceholders);
}

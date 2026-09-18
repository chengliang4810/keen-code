/**
 * Composer draft document model: text segments + inline skill chips.
 * Storage / user bubbles use stable tokens `[[skill:name]]`.
 * Agent 提示词把 Skills 序列化为 Runtime 支持的 `/name` 调用形式。
 */

export type DraftSegment =
  | { type: "text"; text: string }
  | { type: "skill"; name: string };

const SKILL_TOKEN_RE = /\[\[skill:([a-zA-Z0-9_.:-]+)\]\]/g;

/** 当前唯一的内建 Slash 动作，不应在 ACP 历史中还原成 Skill。 */
const BUILTIN_SLASH_NAMES = new Set(["goal"]);

/**
 * Convert agent-form user text (`/skill-name\nbody`) into display tokens
 * (`[[skill:name]]\nbody`) so history bubbles can render chips.
 * Already-tokenized content is left unchanged.
 */
export function hydrateDisplayContent(content: string): string {
  if (!content) return content;
  if (content.includes("[[skill:")) return content;

  const nl = content.indexOf("\n");
  const firstLine = (nl === -1 ? content : content.slice(0, nl)).trim();
  const body = nl === -1 ? "" : content.slice(nl + 1);

  if (!firstLine) return content;

  const parts = firstLine.split(/\s+/).filter(Boolean);
  if (parts.length === 0) return content;
  if (!parts.every((p) => /^\/[a-zA-Z0-9_.:-]+$/.test(p))) return content;

  const names = parts.map((p) => p.slice(1));
  // Require at least one invocable skill; skip pure built-in command lines.
  const skillNames = names.filter(
    (n) => !BUILTIN_SLASH_NAMES.has(n.toLowerCase()),
  );
  if (skillNames.length === 0) return content;
  // Only convert when every first-line token is a skill (not mixed with builtins).
  if (skillNames.length !== names.length) return content;

  const chips = skillNames.map((n) => `[[skill:${n}]]`).join("");
  if (!body) return chips;
  // Preserve body; chips sit before the rest of the message.
  return `${chips}\n${body}`;
}

/**
 * Parse stored content with `[[skill:name]]` tokens into segments.
 * Invalid / incomplete tokens stay as plain text.
 */
export function parseStoredContent(content: string): DraftSegment[] {
  if (!content) return [];
  const segments: DraftSegment[] = [];
  let last = 0;
  const re = new RegExp(SKILL_TOKEN_RE.source, "g");
  let m: RegExpExecArray | null;
  while ((m = re.exec(content)) !== null) {
    if (m.index > last) {
      segments.push({ type: "text", text: content.slice(last, m.index) });
    }
    segments.push({ type: "skill", name: m[1]! });
    last = m.index + m[0].length;
  }
  if (last < content.length) {
    segments.push({ type: "text", text: content.slice(last) });
  }
  return segments;
}

/** Serialize segments back to stored form (`[[skill:name]]` tokens). */
export function serializeStored(segments: DraftSegment[]): string {
  return segments
    .map((s) => (s.type === "text" ? s.text : `[[skill:${s.name}]]`))
    .join("");
}

/**
 * 数字字面量而非 `Node.TEXT_NODE`：本模块是纯规则，必须能在无 DOM 的
 * Vitest node 环境里直接跑。
 */
const TEXT_NODE = 3;
const ELEMENT_NODE = 1;
const BREAK_TAG = "BR";

/**
 * WebKit 的 contenteditable 默认 `defaultParagraphSeparator` 是 `div`，
 * 换行落在 `<div>` / `<p>` 块边界上（粘贴多行、Enter 分段都是这种形态），
 * 不是 `<br>`。块边界必须还原成换行，否则正文被粘成一行。
 */
const BLOCK_TAGS = new Set(["DIV", "P"]);

/**
 * 把编辑器 DOM 折叠成草稿段。
 *
 * - `<br>` 与块边界都产出 `\n`；
 * - 块级元素只在它前面已经走过兄弟节点时补换行，首块不额外加空行；
 * - WebKit 用「只含单个 `<br>` 的块」表示空行，此时不再递归该 `<br>`，
 *   否则一个空行会被算成两个换行；
 * - `data-skill` 宿主产出 skill 段，相邻文本段合并以保持与
 *   {@link parseStoredContent} 一致的规范形式。
 */
export function segmentsFromEditorDom(root: Node): DraftSegment[] {
  const segs: DraftSegment[] = [];
  let started = false;
  const walk = (node: Node) => {
    if (node.nodeType === TEXT_NODE) {
      const text = node.textContent ?? "";
      if (text) segs.push({ type: "text", text });
      return;
    }
    if (node.nodeType !== ELEMENT_NODE) return;
    const he = node as HTMLElement;
    const skill = he.dataset?.skill;
    if (skill) {
      segs.push({ type: "skill", name: skill });
      return;
    }
    if (he.tagName === BREAK_TAG) {
      segs.push({ type: "text", text: "\n" });
      return;
    }
    if (BLOCK_TAGS.has(he.tagName)) {
      const emptyLine =
        he.childNodes.length === 1 && he.childNodes[0]?.nodeName === BREAK_TAG;
      if (started) segs.push({ type: "text", text: "\n" });
      if (!emptyLine) he.childNodes.forEach(walk);
      return;
    }
    he.childNodes.forEach(walk);
  };
  root.childNodes.forEach((child) => {
    walk(child);
    started = true;
  });

  const merged: DraftSegment[] = [];
  for (const s of segs) {
    if (s.type === "text") {
      const last = merged[merged.length - 1];
      if (last?.type === "text") last.text += s.text;
      else merged.push({ type: "text", text: s.text });
    } else {
      merged.push(s);
    }
  }
  return merged;
}

/**
 * Replace `[[skill:name]]` with `/name` in place for one-line previews
 * (queue strip, titles). Keeps surrounding text order — unlike
 * {@link serializeForAgent}, which groups skills first.
 */
export function previewStoredAsSlash(stored: string): string {
  if (!stored) return stored;
  return stored.replace(new RegExp(SKILL_TOKEN_RE.source, "g"), "/$1");
}

/** Empty when there are no skills and no non-whitespace text. */
export function isDraftEmpty(segments: DraftSegment[]): boolean {
  for (const s of segments) {
    if (s.type === "skill") return false;
    if (s.type === "text" && s.text.trim() !== "") return false;
  }
  return true;
}

/**
 * Build the string sent to the agent:
 * - skills in order as `/name`, space-joined
 * - then `\n` + joined text parts (ends trimmed; internal newlines kept)
 */
export function serializeForAgent(segments: DraftSegment[]): string {
  const skillTokens: string[] = [];
  const textParts: string[] = [];
  for (const s of segments) {
    if (s.type === "skill") skillTokens.push(`/${s.name}`);
    else textParts.push(s.text);
  }

  const skillsPart = skillTokens.join(" ");
  // Trim only leading/trailing whitespace; keep internal newlines.
  const textPart = textParts.join("").replace(/^\s+/, "").replace(/\s+$/, "");

  let body: string;
  if (skillsPart && textPart) body = `${skillsPart}\n${textPart}`;
  else if (skillsPart) body = skillsPart;
  else body = textPart;

  return body;
}

/**
 * Replace the active slash range `[slashStart, slashEnd)` with a skill token
 * plus a trailing space.
 */
export function applySkillAtSlash(
  stored: string,
  slashStart: number,
  slashEnd: number,
  skillName: string,
): string {
  const token = `[[skill:${skillName}]] `;
  return stored.slice(0, slashStart) + token + stored.slice(slashEnd);
}

/**
 * Detect an active slash token at the end of `textBeforeCursor`.
 * `/` must be at index 0 or immediately after whitespace.
 * Query is the non-whitespace rest after `/`.
 * Returns null when there is no active slash (e.g. `https://`).
 *
 * Contenteditable almost always serializes a trailing `\n` (from `<br>`).
 * Without trimming, `/目标\n` fails `$` anchor and filtering looks "broken".
 */
export function detectSlashQuery(
  textBeforeCursor: string,
): { start: number; query: string } | null {
  const text = textBeforeCursor
    .replace(/\uFF0F/g, "/")
    .replace(/[\u200B-\u200D\uFEFF\u2060]/g, "")
    .replace(/[\s\u00a0]+$/u, "");
  const m = /(^|[\s])\/([^\s]*)$/u.exec(text);
  if (!m) return null;
  const start = m.index + m[1]!.length;
  return { start, query: m[2]! };
}

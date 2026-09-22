/**
 * Composer 中的结构化 @ mention。
 *
 * mention 的身份、展示文本与发送载体分开保存。编辑器里使用带数据属性的
 * 原子 DOM 节点，草稿持久化使用编码后的结构 token；发送时再还原为 canonical
 * markdown。这样用户输入的普通文本不会因为正则匹配而被伪造成引用。
 */

export type ComposerMentionKind =
  | "file"
  | "directory"
  | "session"
  | "plugin"
  | "skill";

export type ComposerMentionTrigger = "@" | "#" | "$";

export interface ComposerMentionData {
  path?: string;
  sessionId?: string;
  pluginId?: string;
  skillName?: string;
}

export interface ComposerMention {
  id: string;
  kind: ComposerMentionKind;
  label: string;
  value: string;
  markdown: string;
  description?: string;
  data?: ComposerMentionData;
}

export interface ComposerMentionQuery {
  trigger: ComposerMentionTrigger;
  start: number;
  query: string;
  end: number;
}

const MENTION_TOKEN_PREFIX = "[[mention:";
const MENTION_TOKEN_SUFFIX = "]]";

const MENTION_KINDS: ReadonlySet<string> = new Set([
  "file",
  "directory",
  "session",
  "plugin",
  "skill",
]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Validate decoded data before it is accepted as an editor node. */
export function isComposerMention(value: unknown): value is ComposerMention {
  if (!isRecord(value)) return false;
  if (
    typeof value.id !== "string" ||
    !value.id ||
    typeof value.label !== "string" ||
    !value.label ||
    typeof value.value !== "string" ||
    !value.value ||
    typeof value.markdown !== "string" ||
    !value.markdown ||
    typeof value.kind !== "string" ||
    !MENTION_KINDS.has(value.kind)
  ) {
    return false;
  }
  if (value.description !== undefined && typeof value.description !== "string") {
    return false;
  }
  if (value.data !== undefined && !isRecord(value.data)) return false;
  return true;
}

/** Encode a mention for a lossless, DOM-safe draft token. */
export function encodeComposerMention(mention: ComposerMention): string {
  return `${MENTION_TOKEN_PREFIX}${encodeURIComponent(JSON.stringify(mention))}${MENTION_TOKEN_SUFFIX}`;
}

/** Decode only the structured token produced by this module. */
export function decodeComposerMentionToken(token: string): ComposerMention | null {
  if (!token.startsWith(MENTION_TOKEN_PREFIX) || !token.endsWith(MENTION_TOKEN_SUFFIX)) {
    return null;
  }
  const encoded = token.slice(
    MENTION_TOKEN_PREFIX.length,
    token.length - MENTION_TOKEN_SUFFIX.length,
  );
  try {
    const value: unknown = JSON.parse(decodeURIComponent(encoded));
    return isComposerMention(value) ? value : null;
  } catch {
    return null;
  }
}

/** Return the token prefix used by the draft scanner. */
export function composerMentionTokenPrefix(): string {
  return MENTION_TOKEN_PREFIX;
}

/** Build the canonical markdown sent to the Agent for a selected item. */
export function buildComposerMentionMarkdown(
  kind: ComposerMentionKind,
  label: string,
  value: string,
): string {
  const safeLabel = label.replaceAll("\\", "\\\\").replaceAll("]", "\\]").replaceAll("[", "\\[");
  const safeValue = value.replaceAll("\\", "\\\\").replaceAll(">", "\\>");
  switch (kind) {
    case "file":
      return `[${safeLabel}](${safeValue})`;
    case "directory":
      return `[${safeLabel}](${safeValue.replace(/[\\/]+$/u, "")}/)`;
    case "session":
      return safeLabel === safeValue
        ? `#${safeValue}`
        : `[#${safeLabel}](#${safeValue})`;
    case "plugin":
      return `[@${safeLabel}](plugin://${safeValue})`;
    case "skill":
      return `$${safeLabel}`;
  }
}

/** Keep the visible trigger and the canonical mention kind in one mapping. */
export function composerMentionTriggerForKind(
  kind: ComposerMentionKind,
): ComposerMentionTrigger {
  switch (kind) {
    case "session":
      return "#";
    case "skill":
      return "$";
    case "file":
    case "directory":
    case "plugin":
      return "@";
  }
}

/** Filter only real candidates accepted by the active trigger. */
export function filterComposerMentions(
  entries: readonly ComposerMention[],
  query: ComposerMentionQuery,
): ComposerMention[] {
  const needle = query.query.trim().toLocaleLowerCase();
  return entries.filter((entry) => {
    if (composerMentionTriggerForKind(entry.kind) !== query.trigger) return false;
    if (!needle) return true;
    return [entry.label, entry.value, entry.description ?? ""].some((value) =>
      value.toLocaleLowerCase().includes(needle),
    );
  });
}

/**
 * Detect an active @ query without interpreting arbitrary markdown/text as a
 * mention. Only the final token before the caret is eligible.
 */
export function detectComposerMentionQuery(
  textBeforeCaret: string,
): ComposerMentionQuery | null {
  let end = textBeforeCaret.length;
  while (end > 0 && /[\s\u00a0]/u.test(textBeforeCaret[end - 1] ?? "")) end -= 1;
  const triggers: readonly ComposerMentionTrigger[] = ["@", "#", "$"];
  let triggerIndex = -1;
  let trigger: ComposerMentionTrigger | null = null;
  for (const candidate of triggers) {
    const index = textBeforeCaret.lastIndexOf(candidate, end - 1);
    if (index > triggerIndex) {
      triggerIndex = index;
      trigger = candidate;
    }
  }
  if (triggerIndex < 0 || !trigger) return null;
  if (
    triggerIndex > 0 &&
    !/[\s\u00a0]/u.test(textBeforeCaret[triggerIndex - 1] ?? "")
  ) {
    return null;
  }
  const query = textBeforeCaret.slice(triggerIndex + 1, end);
  if (triggers.some((candidate) => query.includes(candidate)) || query.includes("\n")) {
    return null;
  }
  return { trigger, start: triggerIndex, query, end };
}

/** Insert an actual atomic mention span at the current caret. */
export function insertComposerMentionAtCaret(
  root: HTMLElement,
  mention: ComposerMention,
): boolean {
  const selection = window.getSelection();
  if (!selection || !selection.isCollapsed || !selection.anchorNode) return false;
  if (!root.contains(selection.anchorNode)) return false;

  const caret = resolveTextCaret(root, selection.anchorNode, selection.anchorOffset);
  if (!caret) return false;
  const { node: anchor, offset } = caret;
  const text = anchor.textContent ?? "";
  const before = text.slice(0, offset);
  const active = detectComposerMentionQuery(before);
  if (
    !active ||
    active.end !== before.length ||
    active.trigger !== composerMentionTriggerForKind(mention.kind)
  ) {
    return false;
  }

  const range = document.createRange();
  range.setStart(anchor, active.start);
  range.setEnd(anchor, offset);
  range.deleteContents();

  const chip = document.createElement("span");
  chip.className = `composer-mention composer-mention--${mention.kind}`;
  chip.contentEditable = "false";
  chip.dataset.composerMention = encodeComposerMention(mention);
  chip.dataset.mentionId = mention.id;
  chip.dataset.mentionKind = mention.kind;
  chip.dataset.mentionValue = mention.value;
  const visibleTrigger = composerMentionTriggerForKind(mention.kind);
  chip.setAttribute("aria-label", `${visibleTrigger}${mention.label}`);
  chip.textContent = `${visibleTrigger}${mention.label}`;
  const trailing = document.createTextNode(" ");
  const fragment = document.createDocumentFragment();
  fragment.append(chip, trailing);
  range.insertNode(fragment);
  range.setStart(trailing, trailing.length);
  range.collapse(true);
  selection.removeAllRanges();
  selection.addRange(range);
  root.focus();
  return true;
}

export type ComposerMentionDeleteDirection = "backward" | "forward";

/** 判断一个节点是否为 Composer 生成的原子 mention 宿主。 */
export function isComposerMentionElement(
  node: Node | null,
): node is HTMLElement {
  return node?.nodeType === 1 &&
    typeof (node as HTMLElement).dataset?.composerMention === "string" &&
    Boolean((node as HTMLElement).dataset.composerMention);
}

/**
 * 删除光标左右相邻的原子 mention。
 *
 * 浏览器对 contentEditable=false 节点的 Backspace/Delete 行为并不一致：
 * 有的 WebView 只删除可见文字，有的会把光标移进 span。这里先把嵌套
 * DOM 的边界点折叠成“上一个/下一个可见单元”，再用 Range 删除整个宿主，
 * 并把选区留在删除点，避免 mention 残留半个 token。
 */
export function removeComposerMentionAtCaret(
  root: HTMLElement,
  direction: ComposerMentionDeleteDirection,
): boolean {
  const selection = window.getSelection();
  if (!selection || !selection.isCollapsed || !selection.anchorNode) return false;
  if (!root.contains(selection.anchorNode)) return false;

  const inside = nearestComposerMention(root, selection.anchorNode);
  const mention = inside ?? (
    direction === "backward"
      ? previousComposerUnit(root, selection.anchorNode, selection.anchorOffset)
      : nextComposerUnit(root, selection.anchorNode, selection.anchorOffset)
  );
  if (!mention || !isComposerMentionElement(mention)) return false;

  const range = document.createRange();
  range.selectNode(mention);
  range.deleteContents();
  range.collapse(true);
  selection.removeAllRanges();
  selection.addRange(range);
  root.focus();
  return true;
}

/** Return the nearest atomic mention ancestor without escaping the editor. */
function nearestComposerMention(
  root: HTMLElement,
  node: Node,
): HTMLElement | null {
  let current: Node | null = node;
  while (current && current !== root) {
    if (isComposerMentionElement(current)) return current;
    current = current.parentNode;
  }
  return null;
}

/** Normalize an element boundary (including a nested IME caret) to a text node. */
function resolveTextCaret(
  root: HTMLElement,
  node: Node,
  offset: number,
): { node: Text; offset: number } | null {
  if (node.nodeType === 3) {
    return { node: node as Text, offset: Math.max(0, Math.min(offset, node.textContent?.length ?? 0)) };
  }
  if (!root.contains(node) && node !== root) return null;
  const children = Array.from(node.childNodes);
  const before = children[offset - 1];
  const after = children[offset];
  const candidate = after ? firstTextNode(after) : before ? lastTextNode(before) : null;
  if (!candidate) return null;
  return {
    node: candidate,
    offset: after ? 0 : candidate.textContent?.length ?? 0,
  };
}

/** Return the first text descendant, stopping at an atomic mention. */
function firstTextNode(node: Node): Text | null {
  if (isComposerMentionElement(node)) return null;
  if (node.nodeType === 3) return node as Text;
  for (const child of Array.from(node.childNodes)) {
    const text = firstTextNode(child);
    if (text) return text;
  }
  return null;
}

/** Return the last text descendant, stopping at an atomic mention. */
function lastTextNode(node: Node): Text | null {
  if (isComposerMentionElement(node)) return null;
  if (node.nodeType === 3) return node as Text;
  const children = Array.from(node.childNodes);
  for (let i = children.length - 1; i >= 0; i -= 1) {
    const text = lastTextNode(children[i]!);
    if (text) return text;
  }
  return null;
}

/** Find the last visible unit before a DOM boundary. */
function previousComposerUnit(
  root: HTMLElement,
  node: Node,
  offset: number,
): Node | null {
  if (isComposerMentionElement(node)) return node;
  if (node.nodeType === 3) {
    if (offset > 0) return null;
    return previousSiblingUnit(root, node);
  }
  const children = Array.from(node.childNodes);
  for (let i = Math.min(offset, children.length) - 1; i >= 0; i -= 1) {
    const unit = lastComposerUnit(children[i]!);
    if (unit) return unit;
  }
  return previousSiblingUnit(root, node);
}

/** Find the first visible unit after a DOM boundary. */
function nextComposerUnit(
  root: HTMLElement,
  node: Node,
  offset: number,
): Node | null {
  if (isComposerMentionElement(node)) return node;
  if (node.nodeType === 3) {
    if (offset < (node.textContent?.length ?? 0)) return null;
    return nextSiblingUnit(root, node);
  }
  const children = Array.from(node.childNodes);
  for (let i = Math.max(0, offset); i < children.length; i += 1) {
    const unit = firstComposerUnit(children[i]!);
    if (unit) return unit;
  }
  return nextSiblingUnit(root, node);
}

function previousSiblingUnit(root: HTMLElement, node: Node): Node | null {
  let current: Node | null = node;
  while (current && current !== root && current.parentNode) {
    const parent: Node = current.parentNode;
    const index = Array.prototype.indexOf.call(parent.childNodes, current) as number;
    for (let i = index - 1; i >= 0; i -= 1) {
      const unit = lastComposerUnit(parent.childNodes[i]!);
      if (unit) return unit;
    }
    current = parent;
  }
  return null;
}

function nextSiblingUnit(root: HTMLElement, node: Node): Node | null {
  let current: Node | null = node;
  while (current && current !== root && current.parentNode) {
    const parent: Node = current.parentNode;
    const index = Array.prototype.indexOf.call(parent.childNodes, current) as number;
    for (let i = index + 1; i < parent.childNodes.length; i += 1) {
      const unit = firstComposerUnit(parent.childNodes[i]!);
      if (unit) return unit;
    }
    current = parent;
  }
  return null;
}

function firstComposerUnit(node: Node): Node | null {
  if (isComposerMentionElement(node)) return node;
  if (node.nodeType === 3) return node;
  for (const child of Array.from(node.childNodes)) {
    const unit = firstComposerUnit(child);
    if (unit) return unit;
  }
  return null;
}

function lastComposerUnit(node: Node): Node | null {
  if (isComposerMentionElement(node)) return node;
  if (node.nodeType === 3) return node;
  const children = Array.from(node.childNodes);
  for (let i = children.length - 1; i >= 0; i -= 1) {
    const unit = lastComposerUnit(children[i]!);
    if (unit) return unit;
  }
  return null;
}

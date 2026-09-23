/**
 * Single-dollar math guard for chat markdown.
 *
 * 渲染栈开启 `singleDollarTextMath` 后，`$5-$10`、`$HOME ... $PATH` 与
 * `D:\proj\C$\out` 这类普通文本会被 remark-math 误判成行内公式。逐行扫描
 * （跳过代码围栏与行内代码），只把不像公式的 `$` 转义为 `\$`。
 *
 * 移植自 ZCode `packages/ui/src/components/ai-elements/message.tsx` 的
 * `normalizeMessageSingleDollarMath`（Apache-2.0），并增加 Windows 盘符护栏
 * （字母/数字 + `$` + `\` 连续出现视为路径分隔符，见 isWindowsPathDelimiterDollar）。
 */

const markdownFencePattern = /^(?: {0,3})(`{3,}|~{3,})/;
const likelyMathSyntaxPattern = /[\\{}^_=+\-*/<>|()[\]∇∂∫∑√∞≈≠≤≥±×÷πΠα-ωΑ-Ω]/u;
const texCommandPattern = /\\[A-Za-z]+/;
const simpleMathIdentifierPattern =
  /^(?:[A-Za-z]|[a-z][A-Za-z0-9]{1,2}|\d+(?:\.\d+)?)$/;
const compactCurrencyRangePrefixPattern =
  /^(?:\d[\d,]*(?:\.\d+)?|\.\d+)[+\-*/]$/;
const compactCurrencyAmountStartPattern = /^(?:\d|\.\d)/;

interface MarkdownFence {
  marker: string;
  length: number;
}

function getMarkdownFence(line: string): MarkdownFence | null {
  const match = markdownFencePattern.exec(line);
  if (!match) return null;
  const sequence = match[1] ?? "";
  return { marker: sequence[0] ?? "", length: sequence.length };
}

function isEscapedMarkdownCharacter(text: string, index: number): boolean {
  let slashCount = 0;
  for (let cursor = index - 1; cursor >= 0 && text[cursor] === "\\"; cursor--) {
    slashCount++;
  }
  return slashCount % 2 === 1;
}

function isSingleDollarDelimiter(text: string, index: number): boolean {
  return (
    text[index] === "$" &&
    text[index - 1] !== "$" &&
    text[index + 1] !== "$" &&
    !isEscapedMarkdownCharacter(text, index)
  );
}

/**
 * `C$\users`、`\\filesrv\share$\x`：`$` 前是字母/数字、后紧跟路径分隔符，
 * 这是 Windows 共享/盘符命名习惯，绝不会出现 `$` 紧跟反斜杠的合法行内公式开头。
 */
function isWindowsPathDelimiterDollar(text: string, index: number): boolean {
  const before = text[index - 1] ?? "";
  const after = text[index + 1] ?? "";
  return /[A-Za-z0-9]/.test(before) && /[\\/]/.test(after);
}

function findClosingSingleDollarDelimiter(
  text: string,
  startIndex: number,
): number {
  for (let index = startIndex; index < text.length; index++) {
    if (isSingleDollarDelimiter(text, index)) return index;
  }
  return -1;
}

function isLikelySingleDollarMath(content: string): boolean {
  if (!content || content !== content.trim() || /[\r\n]/.test(content)) {
    return false;
  }
  if (texCommandPattern.test(content) || likelyMathSyntaxPattern.test(content)) {
    return true;
  }
  if (!/\s/.test(content) && simpleMathIdentifierPattern.test(content)) {
    return true;
  }
  return false;
}

function isLikelyCompactCurrencyRangeText(
  text: string,
  closingIndex: number,
  content: string,
): boolean {
  if (!compactCurrencyRangePrefixPattern.test(content)) return false;
  return compactCurrencyAmountStartPattern.test(text.slice(closingIndex + 1));
}

function normalizeSingleDollarMathInText(text: string): string {
  if (!text.includes("$")) return text;

  let output = "";
  for (let index = 0; index < text.length; index++) {
    if (!isSingleDollarDelimiter(text, index)) {
      output += text[index];
      continue;
    }
    if (isWindowsPathDelimiterDollar(text, index)) {
      output += "\\$";
      continue;
    }

    const closingIndex = findClosingSingleDollarDelimiter(text, index + 1);
    if (closingIndex === -1) {
      // 行内没有闭合符也要转义:remark-math 的行内公式可以跨一个换行,
      // 本行裸露的 `$` 会与下一行的 `$` 误配成公式(ZCode 原实现漏了这步)。
      output += "\\$";
      continue;
    }

    const content = text.slice(index + 1, closingIndex);
    if (isLikelyCompactCurrencyRangeText(text, closingIndex, content)) {
      // `$5-$10`：紧凑价格区间的第二个 `$` 会被当成闭合符，只转义当前一个。
      output += "\\$";
      continue;
    }
    if (isLikelySingleDollarMath(content)) {
      output += text.slice(index, closingIndex + 1);
      index = closingIndex;
      continue;
    }
    // `$HOME ... $PATH`：只转义当前 `$`，让后续 `$` 继续按文本扫描。
    output += "\\$";
  }
  return output;
}

function normalizeSingleDollarMathOutsideInlineCode(line: string): string {
  let output = "";
  let cursor = 0;

  while (cursor < line.length) {
    const codeStart = line.indexOf("`", cursor);
    if (codeStart === -1) {
      output += normalizeSingleDollarMathInText(line.slice(cursor));
      break;
    }
    output += normalizeSingleDollarMathInText(line.slice(cursor, codeStart));

    let codeFenceEnd = codeStart + 1;
    while (line[codeFenceEnd] === "`") codeFenceEnd++;
    const codeMarker = line.slice(codeStart, codeFenceEnd);
    const codeEnd = line.indexOf(codeMarker, codeFenceEnd);
    if (codeEnd === -1) {
      output += normalizeSingleDollarMathInText(line.slice(codeStart));
      break;
    }
    output += line.slice(codeStart, codeEnd + codeMarker.length);
    cursor = codeEnd + codeMarker.length;
  }
  return output;
}

/** 对整篇 markdown 做 `$` 公式启发式转义；代码围栏与行内代码保持原文。 */
export function normalizeSingleDollarMath(markdown: string): string {
  if (!markdown.includes("$")) return markdown;

  let output = "";
  let cursor = 0;
  let activeFence: MarkdownFence | null = null;

  while (cursor < markdown.length) {
    const newlineIndex = markdown.indexOf("\n", cursor);
    const lineEnd = newlineIndex === -1 ? markdown.length : newlineIndex;
    const line = markdown.slice(cursor, lineEnd);
    const newline = newlineIndex === -1 ? "" : "\n";
    const fence = getMarkdownFence(line);

    if (activeFence) {
      output += line + newline;
      if (
        fence &&
        fence.marker === activeFence.marker &&
        fence.length >= activeFence.length
      ) {
        activeFence = null;
      }
    } else {
      output += normalizeSingleDollarMathOutsideInlineCode(line) + newline;
      if (fence) activeFence = fence;
    }

    cursor = lineEnd + newline.length;
  }
  return output;
}

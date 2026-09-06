/** 变更视图共用的路径、编辑工具和统一差异辅助函数。 */

/** 统一路径分隔符，并移除根目录之外的末尾斜杠。 */
export function normalizePath(path: string): string {
  let p = (path || "").trim().replace(/\\/g, "/");
  if (!p) return "";
  // 保留 UNC、`\\?\` 等 Windows 扩展路径的双斜杠前缀，再折叠内部重复分隔符。
  const hasLeadingDoubleSlash = p.startsWith("//");
  const body = p
    .slice(hasLeadingDoubleSlash ? 2 : 0)
    .replace(/\/{2,}/g, "/");
  p = (hasLeadingDoubleSlash ? "//" : "") + body;
  // Strip trailing slash except roots.
  if (p.length > 1 && p.endsWith("/")) {
    const isRoot =
      p === "/" ||
      p === "//" ||
      /^[a-zA-Z]:\/$/.test(p) ||
      /^\/\/\?\/[a-zA-Z]:\/$/.test(p);
    if (!isRoot) {
      p = p.replace(/\/+$/, "");
    }
  }
  return p;
}

type PathFlavor = "windows" | "posix";

interface PathHint {
  flavor: PathFlavor;
  /** 盘符、UNC 或扩展前缀是不可歧义的 Windows 语法。 */
  explicit: boolean;
}

interface ParsedPath {
  flavor: PathFlavor;
  absolute: boolean;
  rootKind: "drive" | "unc" | "posix" | "relative";
  root: string;
  segments: string[];
}

/** 根据原始路径识别平台语法；相对反斜杠路径只有在没有项目上下文时才决定平台。 */
function pathHint(path: string): PathHint | undefined {
  // 不裁剪路径两端空格；POSIX 文件名允许它们存在，猜测裁剪会造成错误匹配。
  const value = path;
  if (!value) return undefined;
  if (
    /^[A-Za-z]:[\\/]/.test(value) ||
    /^\\\\/.test(value) ||
    /^\/\//.test(value)
  ) {
    return { flavor: "windows", explicit: true };
  }
  // 单斜杠开头明确是 POSIX；反斜杠可以是 POSIX 文件名中的普通字符。
  if (value.startsWith("/")) return { flavor: "posix", explicit: true };
  if (/^[A-Za-z]:/.test(value) || value.includes("\\")) {
    return { flavor: "windows", explicit: false };
  }
  return undefined;
}

/** 按平台规则折叠 `.`、`..` 和重复分隔符，绝不访问文件系统。 */
function normalizeSegments(segments: string[], absolute: boolean): string[] {
  const normalized: string[] = [];
  for (const segment of segments) {
    if (!segment || segment === ".") continue;
    if (segment === "..") {
      const last = normalized[normalized.length - 1];
      if (last && last !== "..") {
        normalized.pop();
      } else if (!absolute) {
        normalized.push(segment);
      }
      continue;
    }
    normalized.push(segment);
  }
  return normalized;
}

/** 解析一个已经确定平台的路径；扩展 Windows 路径映射到同一普通盘符或 UNC 根。 */
function parsePath(path: string, flavor: PathFlavor): ParsedPath | undefined {
  // 不裁剪路径两端空格；它们可能属于合法的 POSIX 文件名。
  const value = path;
  if (!value) return undefined;
  if (flavor === "posix") {
    const absolute = value.startsWith("/");
    const segments = normalizeSegments(value.split("/"), absolute);
    return {
      flavor,
      absolute,
      rootKind: absolute ? "posix" : "relative",
      root: "",
      segments,
    };
  }

  let normalized = value.replace(/\\/g, "/");
  // `C:foo` 是 Windows 驱动器相对路径，语义依赖当前驱动器目录；本匹配器不猜测其含义。
  if (/^[A-Za-z]:(?!\/)/.test(normalized)) return undefined;
  if (normalized.startsWith("//?/")) {
    const extendedBody = normalized.slice(4);
    if (/^UNC\//i.test(extendedBody)) {
      normalized = "//" + extendedBody.slice(4);
    } else {
      normalized = extendedBody;
    }
  }

  if (/^[A-Za-z]:\//.test(normalized)) {
    const drive = normalized.slice(0, 2).toUpperCase();
    return {
      flavor,
      absolute: true,
      rootKind: "drive",
      root: drive,
      segments: normalizeSegments(normalized.slice(3).split("/"), true),
    };
  }
  if (normalized.startsWith("//")) {
    const parts = normalized.slice(2).split("/").filter(Boolean);
    if (parts.length < 2) return undefined;
    const [server, share, ...rest] = parts;
    return {
      flavor,
      absolute: true,
      rootKind: "unc",
      root: `${server}/${share}`,
      segments: normalizeSegments(rest, true),
    };
  }
  if (normalized.startsWith("/")) {
    // Windows rooted paths without a drive are deliberately not equated to POSIX paths.
    return {
      flavor,
      absolute: true,
      rootKind: "relative",
      root: "/",
      segments: normalizeSegments(normalized.slice(1).split("/"), true),
    };
  }
  return {
    flavor,
    absolute: false,
    rootKind: "relative",
    root: "",
    segments: normalizeSegments(normalized.split("/"), false),
  };
}

/** 返回平台相关的大小写归一化分段。 */
function pathPart(path: string, flavor: PathFlavor): string {
  return flavor === "windows" ? path.toLowerCase() : path;
}

/** 返回路径根和分段组成的无歧义比较键。 */
function parsedPathKey(path: ParsedPath): string {
  const prefix = path.flavor === "windows" ? "windows" : "posix";
  const root = pathPart(path.root, path.flavor);
  const segments = path.segments
    .map((segment) => pathPart(segment, path.flavor))
    .join("/");
  if (!path.absolute) return `${prefix}:relative:${segments}`;
  return `${prefix}:absolute:${path.rootKind}:${root}/${segments}`;
}

/** 返回相对路径的比较键，避免把不相关绝对路径按后缀误认为同一文件。 */
function relativePathKey(path: ParsedPath): string {
  const prefix = path.flavor === "windows" ? "windows" : "posix";
  const segments = path.segments
    .map((segment) => pathPart(segment, path.flavor))
    .join("/");
  return `${prefix}:relative:${segments}`;
}

/** 把相对路径按项目根解析为词法路径。 */
function resolveAgainst(project: ParsedPath, relative: ParsedPath): ParsedPath {
  return {
    ...project,
    absolute: project.absolute,
    segments: normalizeSegments(
      [...project.segments, ...relative.segments],
      project.absolute,
    ),
  };
}

/** 在相同根下返回绝对路径相对于项目根的分段；不同根或越出项目时返回空值。 */
function relativeSegments(
  path: ParsedPath,
  project: ParsedPath,
): string[] | undefined {
  if (
    !path.absolute ||
    !project.absolute ||
    path.flavor !== project.flavor ||
    path.rootKind !== project.rootKind ||
    pathPart(path.root, path.flavor) !== pathPart(project.root, project.flavor)
  ) {
    return undefined;
  }
  if (path.segments.length < project.segments.length) return undefined;
  for (let index = 0; index < project.segments.length; index += 1) {
    if (
      pathPart(path.segments[index]!, path.flavor) !==
      pathPart(project.segments[index]!, project.flavor)
    ) {
      return undefined;
    }
  }
  const remainder = path.segments.slice(project.segments.length);
  return remainder.length ? remainder : undefined;
}

/** 为同一项目的路径生成绝对和相对比较键。 */
function projectPathKeys(
  path: string,
  projectPath: string | null | undefined,
): string[] {
  // 保留首尾空格，避免把 POSIX 的 `file ` 猜成 `file`。
  const pathValue = path;
  if (!pathValue) return [];
  const projectValue = projectPath || "";
  // 驱动器相对路径的基准目录未知，不能把它当作项目内普通相对路径。
  if (
    /^[A-Za-z]:(?![\\/])/.test(pathValue) ||
    /^[A-Za-z]:(?![\\/])/.test(projectValue)
  ) {
    return [];
  }
  const projectHint = pathHint(projectValue);
  const valueHint = pathHint(pathValue);
  let flavor: PathFlavor;
  if (projectHint) {
    if (valueHint?.explicit && valueHint.flavor !== projectHint.flavor) {
      return [];
    }
    flavor = projectHint.flavor;
  } else if (valueHint) {
    flavor = valueHint.flavor;
  } else {
    flavor = "posix";
  }
  const parsed = parsePath(pathValue, flavor);
  if (!parsed) return [];
  const project = projectValue ? parsePath(projectValue, flavor) : undefined;
  if (projectValue && !project) return [];

  const keys = new Set<string>([parsedPathKey(parsed)]);
  if (parsed.absolute) {
    const relative = project ? relativeSegments(parsed, project) : undefined;
    if (relative?.length) {
      keys.add(
        relativePathKey({
          flavor,
          absolute: false,
          rootKind: "relative",
          root: "",
          segments: relative,
        }),
      );
    }
  } else if (project) {
    keys.add(parsedPathKey(resolveAgainst(project, parsed)));
  }
  return [...keys];
}

/** 按项目平台词法判断两个路径是否指向同一个文件。 */
export function filePathsMatch(
  leftPath: string | null | undefined,
  rightPath: string | null | undefined,
  projectPath: string | null | undefined,
): boolean {
  const leftKeys = projectPathKeys(leftPath || "", projectPath);
  if (!leftKeys.length) return false;
  const rightKeys = new Set(projectPathKeys(rightPath || "", projectPath));
  return leftKeys.some((key) => rightKeys.has(key));
}

/** 从工具快照中找到与当前文件相同的权威变更；空数组表示没有快照条目。 */
export function fileChangeForPath<T extends { path: string }>(
  changes: readonly T[] | undefined,
  path: string | null | undefined,
  projectPath: string | null | undefined,
): T | undefined {
  if (!changes?.length) return undefined;
  return changes.find((change) =>
    filePathsMatch(change.path, path, projectPath),
  );
}

/** 判断当前运行时工具是否会修改文件。 */
export function isEditToolKind(kind: string | null | undefined): boolean {
  const t = (kind || "").toLowerCase().trim();
  if (!t) return false;
  return t === "write" || t === "edit" || t === "folder_operations";
}

/** 为预览面板生成按行比较的统一差异文本。 */
export function buildUnifiedDiff(
  filePath: string,
  before: string,
  after: string,
  context = 3,
): string {
  const beforeLineCount = lineCount(before);
  const afterLineCount = lineCount(after);
  // 大文件或行数乘积过高时，先剥离完整公共行，避免局部改动退化成整段替换。
  if (
    beforeLineCount + afterLineCount > 50_000 ||
    beforeLineCount * afterLineCount > 2_000_000
  ) {
    return buildLargeUnifiedDiff(
      filePath,
      before,
      after,
      context,
      beforeLineCount,
      afterLineCount,
    );
  }
  return renderUnifiedDiff(filePath, before, after, context, 1, 1);
}

/** 为大文件裁剪公共行和上下文，只对实际变化区执行逐行差异。 */
function buildLargeUnifiedDiff(
  filePath: string,
  before: string,
  after: string,
  context: number,
  beforeLineCount: number,
  afterLineCount: number,
): string {
  // 相同内容仍沿用原有空差异格式，不需要构造裁剪窗口。
  if (before === after) return buildWholeReplacementDiff(filePath, before, after);

  const trimmed = trimCommonLines(before, after);
  const contextLines = Math.max(0, context);
  const prefixContext = Math.min(contextLines, trimmed.prefixLines);
  const suffixContext = Math.min(contextLines, trimmed.suffixLines);
  const beforeWindowStart = rewindLineStart(
    before,
    trimmed.beforeChangeStart,
    prefixContext,
  );
  const afterWindowStart = rewindLineStart(
    after,
    trimmed.afterChangeStart,
    prefixContext,
  );
  const beforeWindowEnd = advanceLineEnd(
    before,
    trimmed.beforeChangeEnd,
    suffixContext,
  );
  const afterWindowEnd = advanceLineEnd(
    after,
    trimmed.afterChangeEnd,
    suffixContext,
  );
  const beforeWindowLineCount =
    beforeLineCount - trimmed.prefixLines - trimmed.suffixLines +
    prefixContext +
    suffixContext;
  const afterWindowLineCount =
    afterLineCount - trimmed.prefixLines - trimmed.suffixLines +
    prefixContext +
    suffixContext;

  // 变化区本身仍然过大时保留有界降级，不能为 LCS 放宽原有预算。
  if (
    beforeWindowLineCount < 0 ||
    afterWindowLineCount < 0 ||
    beforeWindowLineCount + afterWindowLineCount > 50_000
  ) {
    return buildWholeReplacementDiff(filePath, before, after);
  }

  const beforeStartLine = trimmed.prefixLines - prefixContext + 1;
  const afterStartLine = trimmed.prefixLines - prefixContext + 1;
  return renderUnifiedDiff(
    filePath,
    before.slice(beforeWindowStart, beforeWindowEnd),
    after.slice(afterWindowStart, afterWindowEnd),
    contextLines,
    beforeStartLine,
    afterStartLine,
  );
}

/** 按完整行定位首尾公共区间，避免为超大文件分配完整行数组。 */
function trimCommonLines(
  before: string,
  after: string,
): {
  beforeChangeStart: number;
  afterChangeStart: number;
  beforeChangeEnd: number;
  afterChangeEnd: number;
  prefixLines: number;
  suffixLines: number;
} {
  let beforeChangeStart = 0;
  let afterChangeStart = 0;
  let prefixLines = 0;
  while (
    beforeChangeStart < before.length &&
    afterChangeStart < after.length
  ) {
    const beforeLineEnd = nextLineEnd(before, beforeChangeStart);
    const afterLineEnd = nextLineEnd(after, afterChangeStart);
    if (
      !sameTextRange(
        before,
        beforeChangeStart,
        beforeLineEnd,
        after,
        afterChangeStart,
        afterLineEnd,
      )
    ) {
      break;
    }
    beforeChangeStart = beforeLineEnd;
    afterChangeStart = afterLineEnd;
    prefixLines += 1;
  }

  let beforeChangeEnd = before.length;
  let afterChangeEnd = after.length;
  let suffixLines = 0;
  while (
    beforeChangeEnd > beforeChangeStart &&
    afterChangeEnd > afterChangeStart
  ) {
    const beforeLineStart = previousLineStart(before, beforeChangeEnd);
    const afterLineStart = previousLineStart(after, afterChangeEnd);
    if (
      beforeLineStart < beforeChangeStart ||
      afterLineStart < afterChangeStart ||
      !sameTextRange(
        before,
        beforeLineStart,
        beforeChangeEnd,
        after,
        afterLineStart,
        afterChangeEnd,
      )
    ) {
      break;
    }
    beforeChangeEnd = beforeLineStart;
    afterChangeEnd = afterLineStart;
    suffixLines += 1;
  }

  return {
    beforeChangeStart,
    afterChangeStart,
    beforeChangeEnd,
    afterChangeEnd,
    prefixLines,
    suffixLines,
  };
}

/** 返回从指定偏移开始的完整行结束位置，末行可没有 LF。 */
function nextLineEnd(text: string, start: number): number {
  const lineFeed = text.indexOf("\n", start);
  return lineFeed < 0 ? text.length : lineFeed + 1;
}

/** 返回包含指定结束偏移之前内容的最后完整行起点。 */
function previousLineStart(text: string, end: number): number {
  if (end <= 0) return 0;
  const searchEnd = text.charCodeAt(end - 1) === 10 ? end - 2 : end - 1;
  if (searchEnd < 0) return 0;
  return text.lastIndexOf("\n", searchEnd) + 1;
}

/** 比较两个文本区间的原始字符，保留 BOM、CRLF 和末尾换行事实。 */
function sameTextRange(
  left: string,
  leftStart: number,
  leftEnd: number,
  right: string,
  rightStart: number,
  rightEnd: number,
): boolean {
  if (leftEnd - leftStart !== rightEnd - rightStart) return false;
  for (let offset = 0; offset < leftEnd - leftStart; offset += 1) {
    if (left.charCodeAt(leftStart + offset) !== right.charCodeAt(rightStart + offset)) {
      return false;
    }
  }
  return true;
}

/** 从行边界向前移动指定行数。 */
function rewindLineStart(text: string, end: number, count: number): number {
  let start = end;
  for (let index = 0; index < count && start > 0; index += 1) {
    start = previousLineStart(text, start);
  }
  return start;
}

/** 从行边界向后移动指定行数。 */
function advanceLineEnd(text: string, start: number, count: number): number {
  let end = start;
  for (let index = 0; index < count && end < text.length; index += 1) {
    end = nextLineEnd(text, end);
  }
  return end;
}

/** 对给定文本窗口生成统一差异，并以原始文件行号生成差异块头。 */
function renderUnifiedDiff(
  filePath: string,
  before: string,
  after: string,
  context: number,
  oldLineStart: number,
  newLineStart: number,
): string {
  const a = splitLines(before);
  const b = splitLines(after);
  const ops = diffLines(a, b);
  const name = filePath || "file";
  const lines: string[] = [`--- a/${name}`, `+++ b/${name}`];

  // 按上下文范围组织差异块。
  type Row =
    | { t: "eq"; line: string }
    | { t: "del"; line: string }
    | { t: "add"; line: string };
  const rows: Row[] = ops.map((o) => {
    if (o.type === "equal") return { t: "eq", line: o.line };
    if (o.type === "delete") return { t: "del", line: o.line };
    return { t: "add", line: o.line };
  });

  // 标记差异行及其上下文。
  const interesting = new Set<number>();
  rows.forEach((r, i) => {
    if (r.t !== "eq") {
      for (let j = Math.max(0, i - context); j <= Math.min(rows.length - 1, i + context); j++) {
        interesting.add(j);
      }
    }
  });

  if (interesting.size === 0) {
    lines.push("@@ empty diff @@");
    return lines.join("\n");
  }

  let i = 0;
  let oldLine = oldLineStart;
  let newLine = newLineStart;
  while (i < rows.length) {
    if (!interesting.has(i)) {
      const r = rows[i]!;
      if (r.t === "eq") {
        oldLine++;
        newLine++;
      } else if (r.t === "del") {
        oldLine++;
      } else {
        newLine++;
      }
      i++;
      continue;
    }
    // 查找连续的差异块。
    const start = i;
    let end = i;
    while (end + 1 < rows.length && interesting.has(end + 1)) end++;
    // 统计差异块头部需要的行号与数量。
    let oldStart = oldLine;
    let newStart = newLine;
    let oldCount = 0;
    let newCount = 0;
    const body: string[] = [];
    for (let k = start; k <= end; k++) {
      const r = rows[k]!;
      if (r.t === "eq") {
        appendDiffLine(body, " ", r.line);
        oldCount++;
        newCount++;
      } else if (r.t === "del") {
        appendDiffLine(body, "-", r.line);
        oldCount++;
      } else {
        appendDiffLine(body, "+", r.line);
        newCount++;
      }
    }
    // 将旧文件与新文件行号推进到差异块末尾。
    for (let k = start; k <= end; k++) {
      const r = rows[k]!;
      if (r.t === "eq") {
        oldLine++;
        newLine++;
      } else if (r.t === "del") {
        oldLine++;
      } else {
        newLine++;
      }
    }
    lines.push(
      `@@ -${oldCount === 0 ? oldStart - 1 : oldStart},${oldCount} +${newCount === 0 ? newStart - 1 : newStart},${newCount} @@`,
    );
    for (const line of body) lines.push(line);
    i = end + 1;
  }

  return lines.join("\n") + "\n";
}

/** 按真实 LF 边界拆分并保留行尾，使 CRLF 和缺失末尾换行都参与差异比较。 */
function splitLines(text: string): string[] {
  return text.match(/[^\n]*\n|[^\n]+$/g) ?? [];
}

/** 输出真实行内容并为没有 LF 的末行追加标准统一差异标记，不吞掉 CR 或 BOM。 */
function appendDiffLine(output: string[], prefix: string, line: string): void {
  if (line.endsWith("\n")) {
    output.push(prefix + line.slice(0, -1));
  } else {
    output.push(prefix + line, "\\ No newline at end of file");
  }
}

/** 不分配行数组地计算标准统一差异中的真实文件行数。 */
function lineCount(text: string): number {
  if (text.length === 0) return 0;
  let count = text.endsWith("\n") ? 0 : 1;
  for (let index = 0; index < text.length; index += 1) {
    if (text.charCodeAt(index) === 10) count += 1;
  }
  return count;
}

/** 为超多短行文件生成线性内存的整段替换，不构造逐行操作对象或 LCS 表。 */
function buildWholeReplacementDiff(filePath: string, before: string, after: string): string {
  const header = `--- a/${filePath || "file"}\n+++ b/${filePath || "file"}\n`;
  if (before === after) return header + "@@ empty diff @@\n";
  const oldCount = lineCount(before);
  const newCount = lineCount(after);
  const body = (text: string, prefix: string): string => {
    if (!text) return "";
    const prefixed = prefix + text.replace(/\n(?=[\s\S])/g, `\n${prefix}`);
    return text.endsWith("\n") ? prefixed : prefixed + "\n\\ No newline at end of file\n";
  };
  return header + `@@ -${oldCount ? 1 : 0},${oldCount} +${newCount ? 1 : 0},${newCount} @@\n` +
    body(before, "-") + body(after, "+");
}

type DiffOp =
  | { type: "equal"; line: string }
  | { type: "delete"; line: string }
  | { type: "add"; line: string };

/** 使用 LCS 生成预览规模文本的逐行差异。 */
function diffLines(a: string[], b: string[]): DiffOp[] {
  const n = a.length;
  const m = b.length;
  // 过大输入直接降级为整段替换，避免内存失控。
  if (n * m > 2_000_000) {
    return naiveReplaceDiff(a, b);
  }
  const dp: number[][] = Array.from({ length: n + 1 }, () =>
    new Array<number>(m + 1).fill(0),
  );
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      if (a[i] === b[j]) dp[i]![j] = dp[i + 1]![j + 1]! + 1;
      else dp[i]![j] = Math.max(dp[i + 1]![j]!, dp[i]![j + 1]!);
    }
  }
  const out: DiffOp[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ type: "equal", line: a[i]! });
      i++;
      j++;
    } else if (dp[i + 1]![j]! >= dp[i]![j + 1]!) {
      out.push({ type: "delete", line: a[i]! });
      i++;
    } else {
      out.push({ type: "add", line: b[j]! });
      j++;
    }
  }
  while (i < n) {
    out.push({ type: "delete", line: a[i]! });
    i++;
  }
  while (j < m) {
    out.push({ type: "add", line: b[j]! });
    j++;
  }
  return out;
}

/** 将旧文本与新文本表示为整段删除和新增。 */
function naiveReplaceDiff(a: string[], b: string[]): DiffOp[] {
  const out: DiffOp[] = [];
  for (const line of a) out.push({ type: "delete", line });
  for (const line of b) out.push({ type: "add", line });
  return out;
}

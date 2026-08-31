/** 变更视图共用的路径、编辑工具和统一差异辅助函数。 */

/** 统一路径分隔符，并移除根目录之外的末尾斜杠。 */
export function normalizePath(path: string): string {
  let p = (path || "").trim().replace(/\\/g, "/");
  if (!p) return "";
  // Collapse // (but keep leading // for UNC? we treat as local paths)
  p = p.replace(/\/{2,}/g, "/");
  // Windows drive: restore "C:/" style after collapse
  if (/^[a-zA-Z]:\//.test(p) === false && /^[a-zA-Z]:/.test(p)) {
    p = p[0]! + ":" + (p.slice(2).startsWith("/") ? p.slice(2) : "/" + p.slice(2));
  }
  // Strip trailing slash except bare "/" or "C:/"
  if (p.length > 1 && p.endsWith("/")) {
    if (!/^[a-zA-Z]:\/$/.test(p)) {
      p = p.replace(/\/+$/, "");
    }
  }
  return p;
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
  let oldLine = 1;
  let newLine = 1;
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
        body.push(" " + r.line);
        oldCount++;
        newCount++;
      } else if (r.t === "del") {
        body.push("-" + r.line);
        oldCount++;
      } else {
        body.push("+" + r.line);
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
      `@@ -${oldStart},${oldCount} +${newStart},${newCount} @@`,
    );
    lines.push(...body);
    i = end + 1;
  }

  return lines.join("\n");
}

/** 按统一换行符拆分文本。 */
function splitLines(text: string): string[] {
  if (text === "") return [];
  // 末尾换行不产生额外的空内容行。
  const parts = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n").split("\n");
  if (parts.length && parts[parts.length - 1] === "") parts.pop();
  return parts;
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

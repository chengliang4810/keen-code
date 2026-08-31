/** 判断字符串是否为 Unix、Windows 盘符或 UNC 绝对文件路径。 */
export function isAbsoluteFsPath(path: string): boolean {
  return (
    path.startsWith("/") ||
    path.startsWith("\\\\") ||
    /^[A-Za-z]:[\\/]/.test(path)
  );
}

/** 返回文件路径最后一个非空分段，并同时兼容正反斜杠。 */
export function pathBasename(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  const parts = normalized.split("/").filter(Boolean);
  return parts[parts.length - 1] || path;
}

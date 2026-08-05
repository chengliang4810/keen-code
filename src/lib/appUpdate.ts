import type { AppUpdateStatus } from "@/lib/api";

export type AppUpdateAction = "check" | "showProgress" | "install" | "retry";

/** 更新按钮唯一动作来源，避免侧栏、设置页和进度窗行为分叉。 */
export function appUpdateActionFor(
  status: AppUpdateStatus | null,
): AppUpdateAction {
  if (status?.available !== true) return "check";
  if (status.downloadState === "ready") return "install";
  if (status.downloadState === "failed") return "retry";
  return "showProgress";
}

/** 服务端给出总大小时返回稳定的 0-100 下载百分比。 */
export function appUpdateProgressPercent(
  status: AppUpdateStatus | null,
): number | null {
  const total = status?.totalBytes;
  if (!total || total <= 0) return null;
  const downloaded = Math.max(0, status?.downloadedBytes ?? 0);
  return Math.min(100, Math.round((downloaded / total) * 100));
}

/** 以紧凑二进制单位展示下载量。 */
export function formatUpdateBytes(bytes: number, locale: string): string {
  const safeBytes = Number.isFinite(bytes) ? Math.max(0, bytes) : 0;
  const units = ["B", "KB", "MB", "GB"] as const;
  let value = safeBytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  const formatted = new Intl.NumberFormat(locale, {
    maximumFractionDigits: unitIndex === 0 ? 0 : 1,
  }).format(value);
  return `${formatted} ${units[unitIndex]}`;
}

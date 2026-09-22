import type { MessageKey, Vars } from "@/i18n";

type Translator = (key: MessageKey, vars?: Vars) => string;

/**
 * 将侧栏会话时间压缩为 ZCode 风格的相对时间。
 * 无效时间不显示元信息，避免把缺失数据误报为刚刚更新。
 */
export function formatSessionRelativeTime(
  updatedAt: string,
  tr: Translator,
  now = Date.now(),
): string {
  const timestamp = Date.parse(updatedAt);
  if (!Number.isFinite(timestamp)) return "";

  const minutes = Math.max(0, Math.floor((now - timestamp) / 60_000));
  if (minutes < 1) return tr("session.justNow");
  if (minutes < 60) {
    return tr("session.minutesAgo", { minutes });
  }

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return tr("session.hoursAgo", { hours });

  return tr("session.daysAgo", { days: Math.floor(hours / 24) });
}

/** 将消息时间格式化为紧凑的“星期 + 时分”文本。 */
export function formatMessageTime(
  iso: string | null | undefined,
  locale: string,
): string {
  if (!iso) return "";
  const timestamp = Date.parse(iso);
  if (Number.isNaN(timestamp)) return "";
  const date = new Date(timestamp);
  const resolvedLocale = locale === "zh" ? "zh-CN" : "en-US";
  const weekday = new Intl.DateTimeFormat(resolvedLocale, {
    weekday: "short",
  }).format(date);
  const time = new Intl.DateTimeFormat(resolvedLocale, {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(date);
  return locale === "zh" ? `${weekday}${time}` : `${weekday} ${time}`;
}

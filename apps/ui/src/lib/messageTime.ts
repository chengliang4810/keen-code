/** 会话内每条消息都要格式化时间，格式化器按语言复用而不是逐条重建。 */
const weekdayFormatters = new Map<string, Intl.DateTimeFormat>();
const clockFormatters = new Map<string, Intl.DateTimeFormat>();

function cachedFormatter(
  cache: Map<string, Intl.DateTimeFormat>,
  locale: string,
  options: Intl.DateTimeFormatOptions,
): Intl.DateTimeFormat {
  let formatter = cache.get(locale);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat(locale, options);
    cache.set(locale, formatter);
  }
  return formatter;
}

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
  const weekday = cachedFormatter(weekdayFormatters, resolvedLocale, {
    weekday: "short",
  }).format(date);
  const time = cachedFormatter(clockFormatters, resolvedLocale, {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(date);
  return locale === "zh" ? `${weekday}${time}` : `${weekday} ${time}`;
}

import { createT, type Locale } from "@/i18n";

/** Harness token-format.ts 的 K/M 缩写规则；未知与超出 JS 精度的计数不显示为零。 */
export function formatMetricTokens(value: number | null | undefined, locale: Locale, compact: boolean): string | null {
  if (value == null || !Number.isSafeInteger(value) || value < 0) return null;
  if (!compact) return value.toLocaleString(locale === "zh" ? "zh-CN" : locale);
  const scaled = (count: number) => count >= 100 ? String(Math.round(count)) : String(Math.round(count * 10) / 10);
  if (value < 1_000) return String(value);
  if (value < 1_000_000) return `${scaled(value / 1_000)}K`;
  return `${scaled(value / 1_000_000)}M`;
}

/** 与 Harness 一样，用时入口取整秒，超过一分钟显示分秒。 */
export function formatRunDuration(value: number | null | undefined, locale: Locale): string | null {
  if (value == null || !Number.isFinite(value) || value < 0) return null;
  const tr = createT(locale);
  const seconds = Math.floor(value / 1_000);
  return seconds >= 60
    ? tr("chat.turnMetrics.minutes", { minutes: Math.floor(seconds / 60), seconds: String(seconds % 60).padStart(2, "0") })
    : tr("chat.turnMetrics.seconds", { seconds });
}

/** 首 Token 观测保留亚秒精度，不使用 DOM 迟到时间推算。 */
export function formatTurnLatency(durationMs: number): string | null {
  if (!Number.isFinite(durationMs) || durationMs < 0) return null;
  if (durationMs < 1_000) return `${Math.round(durationMs)}ms`;
  if (durationMs < 60_000) {
    const seconds = durationMs / 1_000;
    return `${Number(seconds.toFixed(seconds < 10 ? 2 : 1))}s`;
  }
  return `${Math.floor(durationMs / 60_000)}m ${Math.floor((durationMs % 60_000) / 1_000)}s`;
}

/** Harness message-chrome：10 TPS 起取整，较小值最多保留一位小数。 */
export function formatTokensPerSecond(value: number | null | undefined): string | null {
  if (value == null || !Number.isFinite(value) || value < 0) return null;
  return value >= 10 ? String(Math.round(value)) : String(Math.round(value * 10) / 10);
}

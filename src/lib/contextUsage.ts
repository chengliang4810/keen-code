/** ACP context usage display types and formatters. */

import type { SessionContextUsage } from "@/features/app/models";

export type ContextUsageSource = "known" | "estimated" | "unknown";

export interface ContextUsageDisplay {
  /** Current context tokens reported by ACP, when available. */
  tokens: number | null;
  source: ContextUsageSource;
  /** Chip primary label: "42k", "~12k", or "—". */
  label: string;
  /** Current model context window, when the runtime reports a valid value. */
  contextWindow?: number;
  /** Context usage percentage, clamped to 0–100 for the SVG ring. */
  percentage?: number;
}

/** 清除指定 Session 的上下文用量缓存，不影响其他 Session。 */
export function invalidateSessionContextUsage(
  usageBySession: Map<string, SessionContextUsage>,
  sessionId: string,
): void {
  usageBySession.delete(sessionId);
}

/** Compact token display: 999 / 1.2k / 12k / 1.5M. */
export function formatTokenCount(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "—";
  if (n >= 1_000_000) {
    const value = n / 1_000_000;
    return `${value >= 10 ? Math.round(value) : value.toFixed(1).replace(/\.0$/, "")}M`;
  }
  if (n >= 10_000) return `${Math.round(n / 1000)}k`;
  if (n >= 1000) {
    const value = n / 1000;
    return `${value.toFixed(1).replace(/\.0$/, "")}k`;
  }
  return String(Math.round(n));
}

export function formatContextChipLabel(
  tokens: number | null,
  source: ContextUsageSource,
): string {
  if (tokens == null || source === "unknown") return "—";
  const label = formatTokenCount(tokens);
  return source === "estimated" ? `~${label}` : label;
}

/** Add a valid model context window and percentage to an ACP usage display. */
export function attachContextWindow(
  display: ContextUsageDisplay,
  contextWindow: number | null | undefined,
): ContextUsageDisplay {
  if (
    display.tokens == null ||
    contextWindow == null ||
    !Number.isFinite(contextWindow) ||
    contextWindow <= 0
  ) {
    return display;
  }

  const windowSize = Math.floor(contextWindow);
  if (windowSize <= 0) return display;
  return {
    ...display,
    label: `${display.label} / ${formatTokenCount(windowSize)}`,
    contextWindow: windowSize,
    percentage: Math.min(100, Math.max(0, (display.tokens / windowSize) * 100)),
  };
}

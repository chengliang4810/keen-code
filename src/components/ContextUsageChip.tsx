/** 输入区中的只读上下文用量圆环与悬浮看板。 */

import { Tip } from "@/components/ui/tooltip";
import type { ContextUsageDisplay } from "@/lib/contextUsage";
import type { TaskCacheUsage } from "@/lib/api";

export type ContextUsageChipLabels = {
  /** 读屏与悬浮提示使用的简短名称。 */
  aria: string;
  /** 当前上下文窗口的百分比使用率。 */
  contextUsageRate: string;
  /** 当前任务跨全部轮次的缓存命中率。 */
  taskCacheHitRate: string;
};

type Props = {
  /** 当前应展示的上下文计数。 */
  display: ContextUsageDisplay;
  /** 当前语言的界面文案。 */
  labels: ContextUsageChipLabels;
  /** 从本地请求记录按任务汇总的缓存用量。 */
  taskCacheUsage?: TaskCacheUsage | null;
  /** 禁用悬浮提示，但仍保留只读计数。 */
  disabled?: boolean;
};

/** 以圆环展示上下文占用，悬浮时显示上下文与任务整体缓存率。 */
export function ContextUsageChip({
  display,
  labels,
  taskCacheUsage,
  disabled,
}: Props) {
  const percentage = display.percentage ?? 0;
  const circumference = 2 * Math.PI * 6.5;
  const dashOffset = circumference * (1 - percentage / 100);
  const contextUsageRate = formatContextUsagePercentage(
    display.percentage,
    display.source,
  );
  const cacheHitRate = formatTaskCacheHitRate(taskCacheUsage?.cacheHitRate);
  const accessibleSummary = `${labels.aria}: ${display.label}; ${labels.contextUsageRate}: ${contextUsageRate}; ${labels.taskCacheHitRate}: ${cacheHitRate}`;
  return (
    <div className="ctx-chip">
      <Tip
        className="context-usage-tip"
        label={
          <span className="context-usage-board">
            <span className="context-usage-board__row">
              <span className="context-usage-board__label">{labels.aria}</span>
              <strong className="context-usage-board__value">
                {display.label}
              </strong>
            </span>
            <span className="context-usage-board__row">
              <span className="context-usage-board__label">
                {labels.contextUsageRate}
              </span>
              <strong className="context-usage-board__value">
                {contextUsageRate}
              </strong>
            </span>
            <span className="context-usage-board__row">
              <span className="context-usage-board__label">
                {labels.taskCacheHitRate}
              </span>
              <strong className="context-usage-board__value">
                {cacheHitRate}
              </strong>
            </span>
          </span>
        }
        disabled={disabled}
      >
        <span
          className={
            "chip chip--context" +
            (display.source === "unknown" ? " chip--muted" : "")
          }
          aria-label={accessibleSummary}
          aria-disabled={disabled || undefined}
        >
          <svg
            className="context-ring"
            width="16"
            height="16"
            viewBox="0 0 16 16"
            aria-hidden="true"
          >
            <circle className="context-ring__track" cx="8" cy="8" r="6.5" />
            <circle
              className="context-ring__value"
              cx="8"
              cy="8"
              r="6.5"
              strokeDasharray={circumference}
              strokeDashoffset={dashOffset}
            />
          </svg>
        </span>
      </Tip>
    </div>
  );
}

/** 上下文窗口未知时显示未知；有效比例最多保留一位小数。 */
export function formatContextUsagePercentage(
  percentage: number | null | undefined,
  source: ContextUsageDisplay["source"] = "known",
): string {
  if (
    source === "unknown" ||
    percentage == null ||
    !Number.isFinite(percentage) ||
    percentage < 0 ||
    percentage > 100
  ) {
    return "—";
  }
  const rounded = Math.round(percentage * 10) / 10;
  const value = Number.isInteger(rounded)
    ? rounded.toFixed(0)
    : rounded.toFixed(1);
  return `${source === "estimated" ? "~" : ""}${value}%`;
}

/** 明确零命中显示 0%；缺失或非法的 Provider usage 显示未知。 */
export function formatTaskCacheHitRate(
  rate: number | null | undefined,
): string {
  if (rate == null || !Number.isFinite(rate) || rate < 0 || rate > 1) {
    return "—";
  }
  const percent = Math.round(rate * 1_000) / 10;
  return `${Number.isInteger(percent) ? percent.toFixed(0) : percent.toFixed(1)}%`;
}

/** 输入区中的只读上下文用量圆环。 */

import { Tip } from "@/components/ui/tooltip";
import type { ContextUsageDisplay } from "@/lib/contextUsage";

export type ContextUsageChipLabels = {
  /** 读屏与悬浮提示使用的简短名称。 */
  aria: string;
};

type Props = {
  /** 当前应展示的上下文计数。 */
  display: ContextUsageDisplay;
  /** 当前语言的界面文案。 */
  labels: ContextUsageChipLabels;
  /** 禁用悬浮提示，但仍保留只读计数。 */
  disabled?: boolean;
};

/** 以圆环展示上下文占用，悬浮时显示已用量与容量。 */
export function ContextUsageChip({
  display,
  labels,
  disabled,
}: Props) {
  const percentage = display.percentage ?? 0;
  const circumference = 2 * Math.PI * 6.5;
  const dashOffset = circumference * (1 - percentage / 100);
  return (
    <div className="ctx-chip">
      <Tip
        label={`${labels.aria}: ${display.label}`}
        disabled={disabled}
      >
        <span
          className={
            "chip chip--context" +
            (display.source === "unknown" ? " chip--muted" : "")
          }
          aria-label={`${labels.aria}: ${display.label}`}
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

import { useEffect, useState } from "react";
import type { Locale } from "@/i18n";
import type { AcpGoalProjection } from "@/lib/acp/store";
import {
  IconClose,
  IconRename,
  IconTarget,
  IconTrash,
} from "@/components/icons";

/** 输入框目标栏属性。 */
export interface ComposerGoalProgressProps {
  /** 当前界面语言。 */
  locale: Locale;
  /** ACP Goal 事件归约出的当前目标投影。 */
  goal?: AcpGoalProjection | null;
  /** 打开目标编辑入口。 */
  onEdit: () => void;
  /** 清除当前目标。 */
  onClear: () => void;
  /** 当前 Session 是否仍在执行，用于实时累计目标耗时。 */
  running?: boolean;
}

/** 输入框目标模式标签属性。 */
export interface ComposerGoalChipProps {
  /** 当前界面语言。 */
  locale: Locale;
  /** 清除当前目标。 */
  onClear: () => void;
}

/** 将目标累计耗时格式化为紧凑文本。 */
export function formatGoalElapsed(seconds: number): string {
  const safe = Math.max(0, Math.floor(Number.isFinite(seconds) ? seconds : 0));
  if (safe < 60) return `${safe}s`;
  if (safe < 3600) return `${Math.floor(safe / 60)}m`;
  return `${Math.floor(safe / 3600)}h`;
}

/** 输入框上方的当前目标状态栏。 */
export function ComposerGoalProgress({
  locale,
  goal,
  onEdit,
  onClear,
  running = false,
}: ComposerGoalProgressProps) {
  const current = goal?.goal;
  const [elapsed, setElapsed] = useState(current?.time_used_seconds ?? 0);

  // 仅在当前目标真实执行时每秒刷新一次，空闲时不产生后台活动。
  useEffect(() => {
    const base = current?.time_used_seconds ?? 0;
    setElapsed(base);
    if (!current || current.status !== "active" || !running) return;
    const startedAt = Date.now();
    const timer = window.setInterval(() => {
      setElapsed(base + Math.floor((Date.now() - startedAt) / 1000));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [current?.id, current?.status, current?.time_used_seconds, running]);

  if (!current) return null;
  const zh = locale !== "en";
  const statusLabel =
    current.status === "completed"
      ? zh
        ? "已完成的目标"
        : "Completed goal"
      : current.status === "blocked"
        ? zh
          ? "已阻塞的目标"
          : "Blocked goal"
        : zh
          ? "进行中的目标"
          : "Active goal";
  const objective = current.objective || current.title;

  return (
    <div className={`composer-goal composer-goal--${current.status}`}>
      <IconTarget size={17} />
      <div className="composer-goal__summary" title={objective}>
        <strong>{statusLabel}:</strong>
        <span>{objective}</span>
      </div>
      <span className="composer-goal__elapsed">
        {formatGoalElapsed(elapsed)}
      </span>
      <button
        type="button"
        className="composer-goal__action"
        aria-label={zh ? "编辑目标" : "Edit goal"}
        title={zh ? "编辑目标" : "Edit goal"}
        onClick={onEdit}
      >
        <IconRename size={15} />
      </button>
      <button
        type="button"
        className="composer-goal__action"
        aria-label={zh ? "清除目标" : "Clear goal"}
        title={zh ? "清除目标" : "Clear goal"}
        onClick={onClear}
      >
        <IconTrash size={15} />
      </button>
    </div>
  );
}

/** 输入框工具栏中的目标模式标签。 */
export function ComposerGoalChip({ locale, onClear }: ComposerGoalChipProps) {
  const label = locale === "en" ? "Goal" : "目标";
  const clearLabel = locale === "en" ? "Clear goal" : "清除目标";
  return (
    <div className="composer-goal-chip" aria-label={label}>
      <button
        type="button"
        className="composer-goal-chip__clear"
        aria-label={clearLabel}
        title={clearLabel}
        onClick={onClear}
      >
        <IconClose size={12} />
      </button>
      <span>{label}</span>
    </div>
  );
}

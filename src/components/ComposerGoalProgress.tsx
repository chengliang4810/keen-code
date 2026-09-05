import { Button } from "@/components/ui/button";
import { useEffect, useState } from "react";
import { createT, type Locale } from "@/i18n";
import type { AcpGoalProjection } from "@/lib/acp/store";
import {
  IconAlertTriangle,
  IconCheck,
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
  /** 打开确认弹窗并将当前 active Goal 标记为完成。 */
  onComplete: () => void | Promise<void>;
  /** 打开原因输入并将当前 active Goal 标记为阻塞。 */
  onBlock: () => void;
  /** 清除当前目标。 */
  onClear: () => void;
  /** 当前是否正在提交 Goal 状态转换。 */
  goalTransitionPending?: boolean;
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
  onComplete,
  onBlock,
  onClear,
  goalTransitionPending = false,
  running = false,
}: ComposerGoalProgressProps) {
  const tr = createT(locale);
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
  const statusLabel =
    current.status === "completed"
      ? tr("goal.statusCompleted")
      : current.status === "blocked"
        ? tr("goal.statusBlocked")
        : tr("goal.statusActive");
  const objective = current.objective || current.title;
  const active = current.status === "active";

  return (
    <div className={`composer-goal composer-goal--${current.status}`}>
      <span
        className="sr-only"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        {statusLabel}
      </span>
      <IconTarget size={17} />
      <div className="composer-goal__summary" title={objective}>
        <strong>{statusLabel}:</strong>
        <span>{objective}</span>
      </div>
      <span className="composer-goal__elapsed">
        {formatGoalElapsed(elapsed)}
      </span>
      <Button
        type="button"
        className="composer-goal__action"
        aria-label={tr("goal.edit")}
        title={tr("goal.edit")}
        disabled={goalTransitionPending}
        onClick={onEdit}
      >
        <IconRename size={15} />
      </Button>
      {active ? (
        <>
          <Button
            type="button"
            className="composer-goal__action"
            aria-label={tr("goal.complete")}
            title={tr("goal.complete")}
            disabled={goalTransitionPending}
            aria-busy={goalTransitionPending}
            onClick={onComplete}
          >
            <IconCheck size={15} />
          </Button>
          <Button
            type="button"
            className="composer-goal__action"
            aria-label={tr("goal.block")}
            title={tr("goal.block")}
            disabled={goalTransitionPending}
            aria-busy={goalTransitionPending}
            onClick={onBlock}
          >
            <IconAlertTriangle size={15} />
          </Button>
        </>
      ) : null}
      <Button
        type="button"
        className="composer-goal__action"
        aria-label={tr("goal.clear")}
        title={tr("goal.clear")}
        disabled={goalTransitionPending}
        onClick={onClear}
      >
        <IconTrash size={15} />
      </Button>
    </div>
  );
}

/** 输入框工具栏中的目标模式标签。 */
export function ComposerGoalChip({ locale, onClear }: ComposerGoalChipProps) {
  const tr = createT(locale);
  const label = tr("goal.mode");
  const clearLabel = tr("goal.clear");
  return (
    <Button
      type="button"
      className="composer-goal-chip"
      aria-label={clearLabel}
      title={clearLabel}
      onClick={onClear}
    >
      <span className="composer-goal-chip__icon composer-goal-chip__icon--target">
        <IconTarget size={17} />
      </span>
      <span className="composer-goal-chip__icon composer-goal-chip__icon--clear">
        <IconClose size={12} />
      </span>
      <span>{label}</span>
    </Button>
  );
}

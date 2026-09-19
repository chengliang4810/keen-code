import { Button } from "@/components/ui/button";
import { useEffect, useRef, useState } from "react";
import type { Locale } from "@/i18n";
import type { AcpGoalProjection } from "@/lib/acp/store";
import {
  IconClose,
  IconPause,
  IconPlay,
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
  /** 暂停当前目标对应的运行回合。 */
  onPause: () => void;
  /** 从暂停状态继续执行当前目标。 */
  onResume: () => void;
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

/** 目标耗时只使用 Runtime 持久值与本机已累计的实际运行时长。 */
export function goalElapsedSeconds(
  goal: AcpGoalProjection["goal"],
  storedSeconds = 0,
): number {
  const persisted = goal?.timeUsedSeconds ?? 0;
  return Math.max(persisted, storedSeconds);
}

function goalElapsedStorageKey(goalId: string): string {
  return `keencode:goal-elapsed:${goalId}`;
}

function readStoredGoalElapsed(goalId?: string): number {
  if (!goalId || typeof window === "undefined") return 0;
  try {
    const value = Number(window.localStorage.getItem(goalElapsedStorageKey(goalId)));
    return Number.isFinite(value) && value > 0 ? Math.floor(value) : 0;
  } catch {
    return 0;
  }
}

function storeGoalElapsed(goalId: string, seconds: number): void {
  try {
    window.localStorage.setItem(goalElapsedStorageKey(goalId), String(seconds));
  } catch {
    // 持久化不可用时仍保留当前进程内计时。
  }
}

/** 输入框上方的当前目标状态栏。 */
export function ComposerGoalProgress({
  locale,
  goal,
  onEdit,
  onClear,
  onPause,
  onResume,
  running = false,
}: ComposerGoalProgressProps) {
  const current = goal?.goal ?? null;
  const [elapsed, setElapsed] = useState(() =>
    goalElapsedSeconds(current, readStoredGoalElapsed(current?.id)),
  );
  // 本机累计只增不减：goal_changed 触发的投影刷新不得把显示拉回较小的持久值。
  const elapsedRef = useRef(elapsed);
  const trackedGoalIdRef = useRef(current?.id ?? null);

  // 仅在当前目标真实执行时每秒刷新一次，空闲时不产生后台活动。
  useEffect(() => {
    const seed = goalElapsedSeconds(current, readStoredGoalElapsed(current?.id));
    if (trackedGoalIdRef.current !== (current?.id ?? null)) {
      trackedGoalIdRef.current = current?.id ?? null;
      elapsedRef.current = seed;
    } else {
      elapsedRef.current = Math.max(elapsedRef.current, seed);
    }
    const startedAt = Date.now();
    const anchor = elapsedRef.current;
    const updateElapsed = () => {
      if (running) {
        const next = anchor + Math.floor(Math.max(0, Date.now() - startedAt) / 1000);
        if (next > elapsedRef.current) elapsedRef.current = next;
      }
      setElapsed(elapsedRef.current);
      if (current) storeGoalElapsed(current.id, elapsedRef.current);
    };
    updateElapsed();
    if (!current || current.status !== "active" || !running) return;
    const timer = window.setInterval(updateElapsed, 1000);
    return () => window.clearInterval(timer);
  }, [
    current?.id,
    current?.status,
    current?.createdAtMs,
    current?.timeUsedSeconds,
    running,
  ]);

  if (!current) return null;
  const zh = locale !== "en";
  const statusLabel =
    current.status === "paused" || (current.status === "active" && !running)
      ? zh
        ? "已暂停的目标"
        : "Paused goal"
      : current.status === "completed"
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
    <div className={`composer-goal composer-goal--${current.status === "paused" || (current.status === "active" && !running) ? "paused" : current.status}`}>
      <IconTarget size={17} />
      <div className="composer-goal__summary" title={objective}>
        <strong>{statusLabel}</strong>
        <span>{objective}</span>
        <span className="composer-goal__elapsed">
          · {formatGoalElapsed(elapsed)}
        </span>
      </div>
      {current.status === "active" || current.status === "paused" ? (
        <Button
          type="button"
          className="composer-goal__action"
          aria-label={current.status === "active" && running ? (zh ? "暂停目标" : "Pause goal") : (zh ? "继续目标" : "Resume goal")}
          title={current.status === "active" && running ? (zh ? "暂停目标" : "Pause goal") : (zh ? "继续目标" : "Resume goal")}
          onClick={current.status === "active" && running ? onPause : onResume}
        >
          {current.status === "active" && running ? <IconPause size={15} /> : <IconPlay size={15} />}
        </Button>
      ) : null}
      <Button
        type="button"
        className="composer-goal__action"
        aria-label={zh ? "编辑目标" : "Edit goal"}
        title={zh ? "编辑目标" : "Edit goal"}
        onClick={onEdit}
      >
        <IconRename size={15} />
      </Button>
      <Button
        type="button"
        className="composer-goal__action"
        aria-label={zh ? "清除目标" : "Clear goal"}
        title={zh ? "清除目标" : "Clear goal"}
        onClick={onClear}
      >
        <IconTrash size={15} />
      </Button>
    </div>
  );
}

/** 输入框工具栏中的目标模式标签。 */
export function ComposerGoalChip({ locale, onClear }: ComposerGoalChipProps) {
  const label = locale === "en" ? "Goal" : "目标";
  const clearLabel = locale === "en" ? "Clear goal" : "清除目标";
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

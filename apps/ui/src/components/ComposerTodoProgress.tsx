import { Button } from "@appica/ui-react/button";
import {
  PreviewCard,
  PreviewCardContent,
  PreviewCardTrigger,
} from "@appica/ui-react/preview-card";
import { useMemo, useState } from "react";
import type { Locale } from "@/i18n";
import type { AcpTodoProjection } from "@/lib/acp/store";
import {
  IconCircle,
  IconCircleCheck,
  IconLoader,
} from "@/components/icons";

/** 输入框上方计划卡片支持的稳定 Todo 状态。 */
type ComposerTodoStatus = "completed" | "in_progress" | "pending";

/** 输入框计划卡片属性。 */
export interface ComposerTodoProgressProps {
  /** 当前界面语言。 */
  locale: Locale;
  /** ACP Plan 事件归约出的当前 Todo 投影。 */
  todos?: AcpTodoProjection | null;
  /** 当前 Session 是否仍在执行；终态后的 Todo 不得继续显示运行动画。 */
  running?: boolean;
}

/** 把运行时状态收敛为计划卡片支持的三个展示状态。 */
function normalizeComposerTodoStatus(status: string): ComposerTodoStatus {
  if (status === "completed") return "completed";
  if (status === "in_progress") return "in_progress";
  return "pending";
}

/** 终态后的 in_progress 不得继续显示运行动画。 */
export function composerTodoDisplayStatus(
  status: string,
  running: boolean,
): ComposerTodoStatus {
  return normalizeComposerTodoStatus(
    status === "in_progress" && !running ? "pending" : status,
  );
}

/** 计算计划卡片当前所处的步骤序号。 */
export function composerTodoStep(
  items: AcpTodoProjection["items"],
): number {
  if (items.length === 0) return 0;
  const activeIndex = items.findIndex((item) => item.status === "in_progress");
  if (activeIndex >= 0) return activeIndex + 1;
  const pendingIndex = items.findIndex((item) => item.status !== "completed");
  return pendingIndex >= 0 ? pendingIndex + 1 : items.length;
}

/** 悬浮计划卡片正文；独立导出便于在内容进入 Portal 前保持稳定渲染语义。 */
export function ComposerTodoCardList({
  items,
  running,
  revision,
}: {
  items: AcpTodoProjection["items"];
  running: boolean;
  revision: number;
}) {
  return (
    <ol className="composer-todo__card">
      {items.map((item, index) => {
        const status = composerTodoDisplayStatus(item.status, running);
        return (
          <li
            key={`${revision}:${index}:${item.content}`}
            className={`composer-todo__item composer-todo__item--${status}`}
          >
            <span className="composer-todo__item-icon" aria-hidden>
              {status === "completed" ? (
                <IconCircleCheck size={18} />
              ) : status === "in_progress" ? (
                <IconLoader size={18} />
              ) : (
                <IconCircle size={18} />
              )}
            </span>
            <span className="composer-todo__content">{item.content}</span>
          </li>
        );
      })}
    </ol>
  );
}

/** 显示在输入框上方的当前计划与步骤进度。 */
export function ComposerTodoProgress({
  locale,
  todos,
  running = false,
}: ComposerTodoProgressProps) {
  const items = todos?.items ?? [];
  const step = useMemo(() => composerTodoStep(items), [items]);
  const completed = items.filter((item) => item.status === "completed").length;
  const progress = (completed / items.length) * 100;
  const [open, setOpen] = useState(false);

  if (items.length === 0) return null;

  const stepLabel =
    locale !== "en"
      ? `第 ${step} / ${items.length} 步`
      : `Step ${step} / ${items.length}`;
  return (
    <PreviewCard open={open} onOpenChange={setOpen}>
      <div className="composer-todo" role="status" aria-live="polite">
      <PreviewCardTrigger
        delay={0}
        render={<Button size="md"
            type="button"
            variant="ghost"
            className="composer-todo__step"
            aria-label={stepLabel}
            aria-expanded={open}
          />}
        >
          <svg
            className="composer-todo__progress"
            viewBox="0 0 16 16"
            aria-hidden="true"
          >
            <circle className="composer-todo__progress-track" cx="8" cy="8" r="6" />
            <circle
              className="composer-todo__progress-value"
              cx="8"
              cy="8"
              r="6"
              pathLength="100"
              strokeDasharray={`${progress} 100`}
            />
          </svg>
          <span>{stepLabel}</span>
        </PreviewCardTrigger>
      </div>
      <PreviewCardContent
        side="top"
        align="center"
        arrow={false}
        className="w-[min(29.5rem,80vw)] max-w-none min-w-0 gap-0 border-0 bg-transparent p-0 shadow-none"
      >
        <ComposerTodoCardList
          items={items}
          running={running}
          revision={todos?.revision ?? 0}
        />
      </PreviewCardContent>
    </PreviewCard>
  );
}

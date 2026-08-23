import { Button } from "@/components/ui/button";
import { useMemo, useRef, useState } from "react";
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
}

/** 把运行时状态收敛为计划卡片支持的三个展示状态。 */
function normalizeComposerTodoStatus(status: string): ComposerTodoStatus {
  if (status === "completed") return "completed";
  if (status === "in_progress") return "in_progress";
  return "pending";
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

/** 显示在输入框上方的当前计划与步骤进度。 */
export function ComposerTodoProgress({
  locale,
  todos,
}: ComposerTodoProgressProps) {
  const items = todos?.items ?? [];
  const step = useMemo(() => composerTodoStep(items), [items]);
  const [open, setOpen] = useState(false);
  const panelRef = useRef<HTMLDivElement>(null);

  if (items.length === 0) return null;

  const stepLabel =
    locale !== "en"
      ? `第 ${step} / ${items.length} 步`
      : `Step ${step} / ${items.length}`;
  return (
    <div
      ref={panelRef}
      className={`composer-todo${open ? " is-open" : ""}`}
      role="status"
      aria-live="polite"
      aria-label={stepLabel}
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      onFocus={() => setOpen(true)}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false);
      }}
    >
      <ol className="composer-todo__card" aria-hidden={!open}>
        {items.map((item, index) => {
          const status = normalizeComposerTodoStatus(item.status);
          return (
            <li
              key={`${todos?.revision ?? 0}:${index}:${item.content}`}
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
      <Button
        type="button"
        className="composer-todo__step"
        aria-expanded={open}
      >
        <span>{stepLabel}</span>
      </Button>
    </div>
  );
}

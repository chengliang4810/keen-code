import { Button } from "@appica/ui-react/button";
/**
 * Whole-turn work disclosure.
 *
 * Title: total worked duration · caret. Body holds every timeline unit that
 * precedes the trailing answer. Only mounted for settled turns, so the
 * default state is folded; expanding is direct user control.
 */

import { useMemo, useState, type ReactNode } from "react";
import { createT, type Locale } from "@/i18n";
import { formatProcessingDuration } from "./Thinking";
import { IconChevronRight } from "@/components/icons";

export function TurnWorkGroup({
  durationMs,
  locale,
  children,
}: {
  /** 权威回合工作耗时（毫秒），来自 Journal 锚定或消息持久化字段。 */
  durationMs: number;
  locale: Locale;
  children: ReactNode;
}) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [open, setOpen] = useState(false);

  return (
    <div
      className={"lobe-turn-work" + (open ? " is-open" : "")}
      data-testid="turn-work-group"
    >
      <Button size="md"
        type="button"
        variant="ghost"
        className="lobe-turn-work__trigger"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="lobe-turn-work__title">
          {tr("chat.workedFor", {
            duration: formatProcessingDuration(durationMs, locale),
          })}
        </span>
        <span
          className={"lobe-turn-work__caret" + (open ? " is-open" : "")}
          aria-hidden
        >
          <IconChevronRight size={12} />
        </span>
      </Button>
      {open ? <div className="lobe-turn-work__body">{children}</div> : null}
    </div>
  );
}

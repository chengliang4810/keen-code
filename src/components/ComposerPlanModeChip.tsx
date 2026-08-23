import { Button } from "@/components/ui/button";
import { useMemo } from "react";
import { createT, type Locale } from "@/i18n";
import { IconClose, IconListNumbers } from "@/components/icons";

/** 输入框工具栏中的计划模式开关属性。 */
export interface ComposerPlanModeChipProps {
  /** 当前界面语言。 */
  locale: Locale;
  /** 计划模式是否激活。 */
  active: boolean;
  /** 切换计划模式。 */
  onToggle: () => void;
  /** 会话不可发送时的禁用态。 */
  disabled?: boolean;
}

/** 输入框工具栏中的计划模式开关。 */
export function ComposerPlanModeChip({
  locale,
  active,
  onToggle,
  disabled = false,
}: ComposerPlanModeChipProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const actionLabel = active
    ? tr("composer.planModeOff")
    : tr("composer.planModeToggle");
  return (
    <Button
      type="button"
      className={
        active ? "composer-plan-chip composer-plan-chip--active" : "composer-plan-chip"
      }
      aria-pressed={active}
      aria-label={actionLabel}
      title={actionLabel}
      disabled={disabled}
      onClick={onToggle}
    >
      <span className="composer-plan-chip__icon composer-plan-chip__icon--plan">
        <IconListNumbers size={16} />
      </span>
      {active ? (
        <span className="composer-plan-chip__icon composer-plan-chip__icon--clear">
          <IconClose size={12} />
        </span>
      ) : null}
      <span>{tr("composer.planMode")}</span>
    </Button>
  );
}

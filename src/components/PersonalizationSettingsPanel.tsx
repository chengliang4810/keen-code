import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createT, type Locale } from "@/i18n";

/** 与后端设置校验保持一致，避免提交必然失败的固定提示词。 */
export const CUSTOM_INSTRUCTIONS_MAX_CHARS = 12_000;

export interface PersonalizationSettingsPanelProps {
  /** 最近一次成功持久化的全局自定义指令。 */
  value: string;
  /** 当前界面语言。 */
  locale: Locale;
  /** 保存后端设置；失败时应 reject 并由面板保留草稿。 */
  onSave: (value: string) => Promise<void>;
}

/** 编辑并显式保存不区分项目的全局用户指令。 */
export function PersonalizationSettingsPanel({
  value,
  locale,
  onSave,
}: PersonalizationSettingsPanelProps) {
  const t = useMemo(() => createT(locale), [locale]);
  const persistedValueRef = useRef(value);
  const [draft, setDraft] = useState(value);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);

  // 后端初次加载或其他成功保存完成时同步；用户已有未保存输入则不覆盖。
  useEffect(() => {
    const previousValue = persistedValueRef.current;
    persistedValueRef.current = value;
    setDraft((current) => (current === previousValue ? value : current));
  }, [value]);

  const dirty = draft !== value;
  const save = useCallback(async () => {
    if (!dirty || saving) return;
    setSaving(true);
    setSaveError(false);
    try {
      await onSave(draft);
    } catch {
      setSaveError(true);
    } finally {
      setSaving(false);
    }
  }, [dirty, draft, onSave, saving]);

  return (
    <section
      className="settings-personalization"
      id="settings-anchor-custom-instructions"
    >
      <div className="settings-personalization__header">
        <div className="settings-personalization__heading">
          <h2 className="settings-personalization__title">
            {t("settings.personalization.customInstructions")}
          </h2>
          <p
            className="settings-personalization__description"
            id="settings-custom-instructions-description"
          >
            {t("settings.personalization.description")} {" "}
            <button
              type="button"
              className="settings-personalization__learn-more"
              aria-expanded={helpOpen}
              aria-controls="settings-custom-instructions-help"
              onClick={() => setHelpOpen((open) => !open)}
            >
              {t("settings.personalization.learnMore")}
            </button>
          </p>
        </div>
        <button
          type="button"
          className="btn btn--solid btn--sm settings-personalization__save"
          disabled={!dirty || saving}
          onClick={() => void save()}
        >
          {saving ? t("resources.saving") : t("common.save")}
        </button>
      </div>

      {helpOpen ? (
        <p
          className="settings-personalization__help"
          id="settings-custom-instructions-help"
        >
          {t("settings.personalization.help")}
        </p>
      ) : null}

      <textarea
        className="settings-personalization__textarea"
        value={draft}
        maxLength={CUSTOM_INSTRUCTIONS_MAX_CHARS}
        aria-label={t("settings.personalization.customInstructions")}
        aria-describedby="settings-custom-instructions-description"
        placeholder={t("settings.personalization.placeholder")}
        spellCheck
        onChange={(event) => {
          setDraft(event.target.value);
          setSaveError(false);
        }}
      />

      {saveError ? (
        <p className="settings-personalization__error" role="alert">
          {t("settings.personalization.saveFailed")}
        </p>
      ) : null}
    </section>
  );
}

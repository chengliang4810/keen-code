import { Textarea } from "@/components/ui/textarea";
import { Button } from "@/components/ui/button";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createT, type Locale } from "@/i18n";
import { Switch } from "@/components/ui/switch";

/** 与后端设置校验保持一致，避免提交必然失败的固定提示词。 */
export const CUSTOM_INSTRUCTIONS_MAX_CHARS = 12_000;
/** 与后端长期记忆校验保持一致。 */
export const MEMORY_MD_MAX_CHARS = 200_000;

export interface PersonalizationSettingsPanelProps {
  /** 最近一次成功持久化的全局自定义指令。 */
  value: string;
  /** 当前界面语言。 */
  locale: Locale;
  /** 保存后端设置；失败时应 reject 并由面板保留草稿。 */
  onSave: (value: string) => Promise<void>;
  /** 是否启用本机记忆。 */
  localMemories: boolean;
  /** 保存本机记忆开关。 */
  onLocalMemoriesChange: (value: boolean) => Promise<void>;
  /** 最近一次成功持久化的长期记忆正文。 */
  memoryFile: string;
  /** 保存长期记忆正文；失败时应 reject 并由面板保留草稿。 */
  onMemoryFileSave: (value: string) => Promise<void>;
  /** 删除此电脑上的全部生成记忆。 */
  onMemoriesReset: () => Promise<void>;
}

/** 编辑全局用户指令；失焦后自动保存。 */
export function PersonalizationSettingsPanel({
  value,
  locale,
  onSave,
  localMemories,
  onLocalMemoriesChange,
  memoryFile,
  onMemoryFileSave,
  onMemoriesReset,
}: PersonalizationSettingsPanelProps) {
  const t = useMemo(() => createT(locale), [locale]);
  const persistedValueRef = useRef(value);
  const persistedMemoryRef = useRef(memoryFile);
  const [draft, setDraft] = useState(value);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [memoryDraft, setMemoryDraft] = useState(memoryFile);
  const [memoryBusy, setMemoryBusy] = useState(false);
  const [memoryError, setMemoryError] = useState(false);

  // 后端初次加载或其他成功保存完成时同步；用户已有未保存输入则不覆盖。
  useEffect(() => {
    const previousValue = persistedValueRef.current;
    persistedValueRef.current = value;
    setDraft((current) => (current === previousValue ? value : current));
  }, [value]);

  useEffect(() => {
    const previousValue = persistedMemoryRef.current;
    persistedMemoryRef.current = memoryFile;
    setMemoryDraft((current) => (current === previousValue ? memoryFile : current));
  }, [memoryFile]);

  const save = useCallback(async () => {
    if (draft === value || saving) return;
    setSaving(true);
    setSaveError(false);
    try {
      await onSave(draft);
    } catch {
      setSaveError(true);
    } finally {
      setSaving(false);
    }
  }, [draft, onSave, saving, value]);

  const saveMemory = useCallback(async () => {
    if (memoryDraft === memoryFile || memoryBusy) return;
    setMemoryBusy(true);
    setMemoryError(false);
    try {
      await onMemoryFileSave(memoryDraft);
    } catch {
      setMemoryError(true);
    } finally {
      setMemoryBusy(false);
    }
  }, [memoryBusy, memoryDraft, memoryFile, onMemoryFileSave]);

  return (
    <div className="settings-personalization-stack">
    <section className="settings-personalization" id="settings-anchor-custom-instructions">
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
            <Button
              type="button"
              className="settings-personalization__learn-more"
              aria-expanded={helpOpen}
              aria-controls="settings-custom-instructions-help"
              onClick={() => setHelpOpen((open) => !open)}
            >
              {t("settings.personalization.learnMore")}
            </Button>
          </p>
        </div>
      </div>

      {helpOpen ? (
        <p
          className="settings-personalization__help"
          id="settings-custom-instructions-help"
        >
          {t("settings.personalization.help")}
        </p>
      ) : null}

      <Textarea
        className="settings-personalization__textarea"
        value={draft}
        maxLength={CUSTOM_INSTRUCTIONS_MAX_CHARS}
        aria-label={t("settings.personalization.customInstructions")}
        aria-describedby="settings-custom-instructions-description"
        placeholder={t("settings.personalization.placeholder")}
        disabled={saving}
        spellCheck
        onChange={(event) => {
          setDraft(event.target.value);
          setSaveError(false);
        }}
        onBlur={() => {
          void save();
        }}
      />

      {saveError ? (
        <p className="settings-personalization__error" role="alert">
          {t("settings.personalization.saveFailed")}
        </p>
      ) : null}
    </section>

    <section className="settings-personalization" id="settings-anchor-local-memories">
      <div className="settings-personalization__header">
        <div className="settings-personalization__heading">
          <h2 className="settings-personalization__title">
            {t("settings.personalization.memories")}
          </h2>
          <p className="settings-personalization__description">
            {t("settings.personalization.memoriesDescription")}
          </p>
        </div>
      </div>

      <div className="settings-card settings-personalization__memory-card">
        <div className="settings-row">
          <div className="settings-row__text">
            <div className="settings-row__label">
              {t("settings.personalization.enableMemories")}
            </div>
            <div className="settings-row__desc">
              {t("settings.personalization.enableMemoriesDescription")}
            </div>
          </div>
          <Switch
            type="button"
            checked={localMemories}
            aria-label={t("settings.personalization.enableMemories")}
            disabled={memoryBusy}
            className={"ext-switch" + (localMemories ? " is-on" : "")}
            onClick={(event) => event.stopPropagation()}
            onCheckedChange={async (value) => {
              setMemoryBusy(true);
              setMemoryError(false);
              try {
                await onLocalMemoriesChange(value === true);
              } catch {
                setMemoryError(true);
              } finally {
                setMemoryBusy(false);
              }
            }}
          />
        </div>
        <div className="settings-row">
          <div className="settings-row__text">
            <div className="settings-row__label">
              {t("settings.personalization.deleteMemories")}
            </div>
          </div>
          <Button
            type="button"
            className="btn btn--danger btn--sm"
            disabled={memoryBusy}
            onClick={async () => {
              if (!window.confirm(t("settings.personalization.deleteMemoriesConfirm"))) return;
              setMemoryBusy(true);
              setMemoryError(false);
              try {
                await onMemoriesReset();
                persistedMemoryRef.current = "";
                setMemoryDraft("");
              } catch {
                setMemoryError(true);
              } finally {
                setMemoryBusy(false);
              }
            }}
          >
            {t("settings.personalization.deleteMemories")}
          </Button>
        </div>
      </div>

      {localMemories ? (
        <Textarea
          className="settings-personalization__textarea"
          value={memoryDraft}
          maxLength={MEMORY_MD_MAX_CHARS}
          aria-label={t("settings.personalization.longTermMemory")}
          disabled={memoryBusy}
          spellCheck
          onChange={(event) => {
            setMemoryDraft(event.target.value);
            setMemoryError(false);
          }}
          onBlur={() => {
            void saveMemory();
          }}
        />
      ) : null}
      {memoryError ? (
        <p className="settings-personalization__error" role="alert">
          {t("settings.personalization.memoriesFailed")}
        </p>
      ) : null}
    </section>
    </div>
  );
}

import { Textarea } from "@/components/ui/textarea";
import { Button } from "@/components/ui/button";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createT, type Locale } from "@/i18n";
import { Switch } from "@/components/ui/switch";
import type { MemoryStatus } from "@/lib/api";

/** 与后端设置校验保持一致，避免提交必然失败的固定提示词。 */
export const CUSTOM_INSTRUCTIONS_MAX_CHARS = 12_000;
/** 与后端长期记忆校验保持一致。 */
export const MEMORY_MD_MAX_CHARS = 200_000;

/** 本机记忆状态卡片支持的系统操作。 */
type MemoryStatusAction = "reveal" | "copy";

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
  /** 当前后端返回的本机记忆状态；为空表示尚未读取或读取失败。 */
  memoryStatus: MemoryStatus | null;
  /** 是否正在按需读取本机记忆状态。 */
  memoryStatusLoading: boolean;
  /** 最近一次读取本机记忆状态是否失败。 */
  memoryStatusError: boolean;
  /** 每次进入个性化设置时按需刷新本机记忆状态。 */
  onRefreshMemoryStatus: () => Promise<void>;
  /** 在系统文件管理器中显示记忆根目录。 */
  onRevealMemoryRoot: () => Promise<void>;
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
  memoryStatus,
  memoryStatusLoading,
  memoryStatusError,
  onRefreshMemoryStatus,
  onRevealMemoryRoot,
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
  /** 当前正在执行的记忆根目录操作，避免重复触发系统调用。 */
  const [memoryStatusAction, setMemoryStatusAction] =
    useState<MemoryStatusAction | null>(null);
  /** 记忆根目录查看或复制失败时显示的操作错误。 */
  const [memoryStatusActionError, setMemoryStatusActionError] = useState(false);
  /** 最近一次记忆根目录操作成功的类型。 */
  const [memoryStatusActionSuccess, setMemoryStatusActionSuccess] =
    useState<MemoryStatusAction | null>(null);

  const memoryRoot = memoryStatus?.root.trim() ?? "";
  /** 用于在根目录或面板生命周期变化后丢弃迟到的操作结果。 */
  const memoryStatusActionRequestRef = useRef(0);
  const memoryRootRef = useRef(memoryRoot);
  memoryRootRef.current = memoryRoot;

  /** 个性化面板每次挂载时读取一次状态；不建立常驻轮询。 */
  useEffect(() => {
    // 消费宿主 Promise，避免宿主异常导致未处理的异步拒绝；状态错误由宿主投影。
    void onRefreshMemoryStatus().catch(() => {});
  }, [onRefreshMemoryStatus]);

  /** 根目录变化后清理旧操作反馈，避免把旧路径的结果展示给新状态。 */
  useEffect(() => {
    setMemoryStatusActionSuccess(null);
    setMemoryStatusActionError(false);
  }, [memoryRoot]);

  /** 面板卸载时使未完成的根目录操作结果失效。 */
  useEffect(
    () => () => {
      memoryStatusActionRequestRef.current += 1;
    },
    [],
  );

  // 后端初次加载或其他成功保存完成时同步；用户已有未保存输入则不覆盖。
  useEffect(() => {
    const previousValue = persistedValueRef.current;
    persistedValueRef.current = value;
    setDraft((current) => (current === previousValue ? value : current));
  }, [value]);

  useEffect(() => {
    const previousValue = persistedMemoryRef.current;
    persistedMemoryRef.current = memoryFile;
    setMemoryDraft((current) =>
      current === previousValue ? memoryFile : current,
    );
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

  /** 执行记忆根目录的查看或复制操作，并在面板内反馈结果。 */
  const runMemoryStatusAction = useCallback(
    async (action: MemoryStatusAction) => {
      if (!memoryRoot || memoryStatusAction) return;
      const requestId = ++memoryStatusActionRequestRef.current;
      const requestedRoot = memoryRoot;
      setMemoryStatusAction(action);
      setMemoryStatusActionError(false);
      setMemoryStatusActionSuccess(null);
      try {
        if (action === "reveal") {
          await onRevealMemoryRoot();
        } else {
          await navigator.clipboard.writeText(requestedRoot);
        }
        if (
          requestId === memoryStatusActionRequestRef.current &&
          memoryRootRef.current === requestedRoot
        ) {
          setMemoryStatusActionSuccess(action);
        }
      } catch {
        if (
          requestId === memoryStatusActionRequestRef.current &&
          memoryRootRef.current === requestedRoot
        ) {
          setMemoryStatusActionError(true);
        }
      } finally {
        if (requestId === memoryStatusActionRequestRef.current) {
          setMemoryStatusAction(null);
        }
      }
    },
    [memoryRoot, memoryStatusAction, onRevealMemoryRoot],
  );

  return (
    <div className="settings-personalization-stack">
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
              {t("settings.personalization.description")}{" "}
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

      <section
        className="settings-personalization"
        id="settings-anchor-local-memories"
      >
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
                if (
                  !window.confirm(
                    t("settings.personalization.deleteMemoriesConfirm"),
                  )
                )
                  return;
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

        <div
          className="settings-card settings-personalization__memory-status"
          aria-busy={memoryStatusLoading || memoryStatusAction !== null}
        >
          {memoryStatus ? (
            <>
              {memoryStatusLoading ? (
                <div className="settings-row settings-row--stack">
                  <div className="settings-row__text">
                    <div
                      className="settings-row__desc"
                      role="status"
                      aria-live="polite"
                    >
                      {t("settings.personalization.memoryStatusLoading")}
                    </div>
                  </div>
                </div>
              ) : null}
              {memoryStatusError ? (
                <div
                  className="settings-row settings-row--stack"
                  role="alert"
                  aria-live="polite"
                >
                  <div className="settings-row__text">
                    <div className="settings-personalization__error">
                      {t("settings.personalization.memoryStatusRefreshFailed")}
                    </div>
                  </div>
                </div>
              ) : null}
              <div className="settings-row">
                <div className="settings-row__text">
                  <div
                    className="settings-row__label"
                    id="settings-memory-status-title"
                  >
                    {t("settings.personalization.memoryStatus")}
                  </div>
                  <div className="settings-row__desc">
                    {t(
                      memoryStatus.enabled
                        ? "settings.personalization.memoryStatusEnabled"
                        : "settings.personalization.memoryStatusDisabled",
                    )}
                  </div>
                </div>
              </div>
              <div className="settings-row settings-row--stack">
                <div className="settings-personalization__memory-status-grid">
                  <div className="settings-row__text">
                    <div className="settings-row__label">
                      {t("settings.personalization.memoryStatusCount")}
                    </div>
                    <div className="settings-row__desc">
                      {memoryStatus.memoryCount}
                    </div>
                  </div>
                  <div className="settings-row__text">
                    <div className="settings-row__label">
                      {t("settings.personalization.memoryStatusExtraction")}
                    </div>
                    <div className="settings-row__desc">
                      {t(
                        memoryStatus.running
                          ? "settings.personalization.memoryStatusRunning"
                          : "settings.personalization.memoryStatusIdle",
                      )}
                    </div>
                  </div>
                </div>
              </div>
              <div className="settings-row settings-row--stack">
                <div className="settings-row__text">
                  <div className="settings-row__label">
                    {t("settings.personalization.memoryStatusRoot")}
                  </div>
                  <code className="settings-row__hint" title={memoryRoot}>
                    {memoryRoot || "—"}
                  </code>
                </div>
                <div
                  className="settings-personalization__memory-status-actions"
                  aria-busy={memoryStatusAction !== null}
                >
                  <Button
                    type="button"
                    className="btn btn--ghost btn--sm"
                    aria-busy={memoryStatusAction === "reveal"}
                    disabled={!memoryRoot || memoryStatusAction !== null}
                    onClick={() => void runMemoryStatusAction("reveal")}
                  >
                    {t("settings.personalization.memoryStatusViewRoot")}
                  </Button>
                  <Button
                    type="button"
                    className="btn btn--ghost btn--sm"
                    aria-busy={memoryStatusAction === "copy"}
                    disabled={!memoryRoot || memoryStatusAction !== null}
                    onClick={() => void runMemoryStatusAction("copy")}
                  >
                    {t("settings.personalization.memoryStatusCopyRoot")}
                  </Button>
                </div>
                {memoryStatusActionSuccess ? (
                  <p
                    className="settings-row__desc"
                    role="status"
                    aria-live="polite"
                  >
                    {t(
                      memoryStatusActionSuccess === "reveal"
                        ? "settings.personalization.memoryStatusRootRevealed"
                        : "settings.personalization.memoryStatusRootCopied",
                    )}
                  </p>
                ) : null}
                {memoryStatusActionError ? (
                  <p className="settings-personalization__error" role="alert">
                    {t("settings.personalization.memoryStatusActionFailed")}
                  </p>
                ) : null}
              </div>
            </>
          ) : memoryStatusLoading ? (
            <div className="settings-row">
              <div className="settings-row__text">
                <div
                  className="settings-row__label"
                  id="settings-memory-status-title"
                >
                  {t("settings.personalization.memoryStatus")}
                </div>
                <div
                  className="settings-row__desc"
                  role="status"
                  aria-live="polite"
                >
                  {t("settings.personalization.memoryStatusLoading")}
                </div>
              </div>
            </div>
          ) : (
            <div className="settings-row">
              <div className="settings-row__text">
                <div
                  className="settings-row__label"
                  id="settings-memory-status-title"
                >
                  {t("settings.personalization.memoryStatus")}
                </div>
                <div
                  className={
                    memoryStatusError
                      ? "settings-personalization__error"
                      : "settings-row__desc"
                  }
                  role={memoryStatusError ? "alert" : "status"}
                >
                  {t("settings.personalization.memoryStatusUnavailable")}
                </div>
              </div>
            </div>
          )}
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

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import type { SettingsRouteSettings } from "@/features/app/SettingsRoute";

const DEFAULT_TERMINAL_FONT_FAMILY =
  'ui-monospace, "SFMono-Regular", Menlo, Monaco, Consolas, monospace';

export interface UseAppSettingsOptions {
  /** 等待应用启动页结束后再从 Tauri 恢复设置。 */
  appBooting: boolean;
  /** 常规设置保存失败时由外层决定 toast/banner 展示方式。 */
  onSaveError?: (message: string) => void;
  /** 设置操作成功后的短提示。 */
  showToast: (message: string, durationMs?: number) => void;
}

export interface AppSettingsController extends SettingsRouteSettings {
  /** 更新下载源属于 AppSettings，但在设置页按 update 域呈现。 */
  appUpdateDownloadSource: api.AppUpdateDownloadSource;
  onAppUpdateDownloadSource: (
    value: api.AppUpdateDownloadSource,
  ) => void;
  /** null 表示设置尚未从本地恢复。 */
  autoArchiveConversations: boolean | null;
  archiveRetentionDays: number;
  onAutoArchiveConversations: (value: boolean) => void;
  onArchiveRetentionDays: (value: number) => void;
}

interface PersistedSettingCallbacks {
  rollback: () => void;
  onSaved?: (settings: api.AppSettings) => void;
}

/**
 * 管理应用级设置的唯一状态源：启动恢复、乐观更新、保存回滚都在这里完成。
 * 归档会话的自动过期扫描仍由工作区控制器负责，因为它依赖 sessions 投影。
 */
export function useAppSettings({
  appBooting,
  onSaveError,
  showToast,
}: UseAppSettingsOptions): AppSettingsController {
  const [locale, setLocale] = useState<Locale>("zh");
  const [chromeHardwareAcceleration, setChromeHardwareAcceleration] =
    useState(true);
  const [customInstructions, setCustomInstructions] = useState("");
  const [memoryFile, setMemoryFile] = useState("");
  const [localMemories, setLocalMemories] = useState(true);
  const [taskNotifications, setTaskNotifications] = useState(true);
  const [notificationSound, setNotificationSound] = useState(true);
  const [autoArchiveConversations, setAutoArchiveConversations] = useState<
    boolean | null
  >(null);
  const [archiveRetentionDays, setArchiveRetentionDays] = useState(7);
  const [appUpdateDownloadSource, setAppUpdateDownloadSource] =
    useState<api.AppUpdateDownloadSource>("auto");
  const [keepComputerAwake, setKeepComputerAwake] = useState(true);
  const [backgroundAgentLimit, setBackgroundAgentLimit] = useState(10);
  const [terminalFontFamily, setTerminalFontFamily] = useState(
    DEFAULT_TERMINAL_FONT_FAMILY,
  );
  const [terminalShell, setTerminalShell] = useState<api.TerminalShell>("auto");
  const [terminalShellOptions, setTerminalShellOptions] = useState<
    api.TerminalShellOption[]
  >([]);
  const [projectDirectory, setProjectDirectory] = useState("");

  const onSaveErrorRef = useRef(onSaveError);
  onSaveErrorRef.current = onSaveError;
  const showToastRef = useRef(showToast);
  showToastRef.current = showToast;
  const tr = useMemo(() => createT(locale), [locale]);

  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  const reportSaveError = useCallback(() => {
    onSaveErrorRef.current?.(tr("settings.saveFailed"));
  }, [tr]);

  /** 所有 AppSettings 保存都经过同一个回滚/错误出口。 */
  const persistSetting = useCallback(
    (
      patch: api.AppSettingsPatch,
      { rollback, onSaved }: PersistedSettingCallbacks,
    ) => {
      void api
        .settingsSet(patch)
        .then((saved) => onSaved?.(saved))
        .catch(() => {
          rollback();
          reportSaveError();
        });
    },
    [reportSaveError],
  );

  // 从本地设置文件恢复常规选项；各资源独立加载，单项失败不阻塞其余设置。
  useEffect(() => {
    if (appBooting || !api.isTauri()) return;
    let active = true;
    void api
      .settingsGet()
      .then((settings) => {
        if (!active) return;
        setChromeHardwareAcceleration(settings.chromeHardwareAcceleration);
        setTaskNotifications(settings.taskNotifications);
        setNotificationSound(settings.notificationSound);
        setAppUpdateDownloadSource(settings.appUpdateDownloadSource);
        setKeepComputerAwake(settings.keepComputerAwake);
        setBackgroundAgentLimit(settings.backgroundAgentLimit);
        setTerminalFontFamily(settings.terminalFontFamily);
        setTerminalShell(settings.terminalShell);
        setProjectDirectory(settings.projectDirectory);
        setLocalMemories(settings.localMemories);
        setAutoArchiveConversations(settings.autoArchiveConversations);
        setArchiveRetentionDays(settings.archiveRetentionDays);
        setLocale(settings.interfaceLanguage);
      })
      .catch(() => {});
    void api
      .terminalShellsList()
      .then((options) => {
        if (active) setTerminalShellOptions(options);
      })
      .catch(() => {});
    void api
      .customInstructionsGet()
      .then((value) => {
        if (active) setCustomInstructions(value);
      })
      .catch(() => {});
    void api
      .memoriesGet()
      .then((value) => {
        if (active) setMemoryFile(value);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [appBooting]);

  const onLocaleChange = useCallback(
    (value: Locale) => {
      const previous = locale;
      setLocale(value);
      persistSetting(
        { interfaceLanguage: value },
        { rollback: () => setLocale(previous) },
      );
    },
    [locale, persistSetting],
  );

  const onCustomInstructionsSave = useCallback(async (value: string) => {
    const saved = await api.customInstructionsSet(value);
    setCustomInstructions(saved);
  }, []);

  const onLocalMemoriesChange = useCallback(async (value: boolean) => {
    const saved = await api.settingsSet({ localMemories: value });
    setLocalMemories(saved.localMemories);
  }, []);

  const onMemoryFileSave = useCallback(async (value: string) => {
    const saved = await api.memoriesSet(value);
    setMemoryFile(saved);
  }, []);

  const onMemoriesReset = useCallback(async () => {
    await api.memoriesReset();
    setMemoryFile("");
    showToastRef.current(tr("settings.personalization.deleteMemoriesDone"));
  }, [tr]);

  const onChromeHardwareAcceleration = useCallback(
    (value: boolean) => {
      const previous = chromeHardwareAcceleration;
      setChromeHardwareAcceleration(value);
      persistSetting(
        { chromeHardwareAcceleration: value },
        { rollback: () => setChromeHardwareAcceleration(previous) },
      );
    },
    [chromeHardwareAcceleration, persistSetting],
  );

  const onTaskNotifications = useCallback(
    (value: boolean) => {
      const previous = taskNotifications;
      setTaskNotifications(value);
      persistSetting(
        { taskNotifications: value },
        { rollback: () => setTaskNotifications(previous) },
      );
    },
    [persistSetting, taskNotifications],
  );

  const onNotificationSound = useCallback(
    (value: boolean) => {
      const previous = notificationSound;
      setNotificationSound(value);
      persistSetting(
        { notificationSound: value },
        { rollback: () => setNotificationSound(previous) },
      );
    },
    [notificationSound, persistSetting],
  );

  const onAppUpdateDownloadSource = useCallback(
    (value: api.AppUpdateDownloadSource) => {
      const previous = appUpdateDownloadSource;
      setAppUpdateDownloadSource(value);
      persistSetting(
        { appUpdateDownloadSource: value },
        { rollback: () => setAppUpdateDownloadSource(previous) },
      );
    },
    [appUpdateDownloadSource, persistSetting],
  );

  const onKeepComputerAwake = useCallback(
    (value: boolean) => {
      const previous = keepComputerAwake;
      setKeepComputerAwake(value);
      persistSetting(
        { keepComputerAwake: value },
        { rollback: () => setKeepComputerAwake(previous) },
      );
    },
    [keepComputerAwake, persistSetting],
  );

  const onBackgroundAgentLimit = useCallback(
    (value: number) => {
      const previous = backgroundAgentLimit;
      setBackgroundAgentLimit(value);
      persistSetting(
        { backgroundAgentLimit: value },
        {
          rollback: () => setBackgroundAgentLimit(previous),
          onSaved: (saved) => setBackgroundAgentLimit(saved.backgroundAgentLimit),
        },
      );
    },
    [backgroundAgentLimit, persistSetting],
  );

  const onTerminalFontFamily = useCallback(
    (value: string) => {
      const previous = terminalFontFamily;
      setTerminalFontFamily(value);
      persistSetting(
        { terminalFontFamily: value },
        { rollback: () => setTerminalFontFamily(previous) },
      );
    },
    [persistSetting, terminalFontFamily],
  );

  const onTerminalShell = useCallback(
    (value: api.TerminalShell) => {
      const previous = terminalShell;
      setTerminalShell(value);
      persistSetting(
        { terminalShell: value },
        { rollback: () => setTerminalShell(previous) },
      );
    },
    [persistSetting, terminalShell],
  );

  const onProjectDirectoryChoose = useCallback(async () => {
    const path = await api.pickDirectory();
    if (!path) return;
    const previous = projectDirectory;
    setProjectDirectory(path);
    try {
      const saved = await api.settingsSet({ projectDirectory: path });
      setProjectDirectory(saved.projectDirectory);
    } catch {
      setProjectDirectory(previous);
      reportSaveError();
    }
  }, [projectDirectory, reportSaveError]);

  const onProjectDirectoryReset = useCallback(async () => {
    const previous = projectDirectory;
    try {
      const path = await api.projectDefaultDirectory();
      setProjectDirectory(path);
      const saved = await api.settingsSet({ projectDirectory: path });
      setProjectDirectory(saved.projectDirectory);
    } catch {
      setProjectDirectory(previous);
      reportSaveError();
    }
  }, [projectDirectory, reportSaveError]);

  const onAutoArchiveConversations = useCallback(
    (value: boolean) => {
      const previous = autoArchiveConversations;
      setAutoArchiveConversations(value);
      persistSetting(
        { autoArchiveConversations: value },
        { rollback: () => setAutoArchiveConversations(previous) },
      );
    },
    [autoArchiveConversations, persistSetting],
  );

  const onArchiveRetentionDays = useCallback(
    (value: number) => {
      const previous = archiveRetentionDays;
      setArchiveRetentionDays(value);
      persistSetting(
        { archiveRetentionDays: value },
        { rollback: () => setArchiveRetentionDays(previous) },
      );
    },
    [archiveRetentionDays, persistSetting],
  );

  return {
    locale,
    onLocaleChange,
    chromeHardwareAcceleration,
    onChromeHardwareAcceleration,
    taskNotifications,
    onTaskNotifications,
    notificationSound,
    onNotificationSound,
    keepComputerAwake,
    onKeepComputerAwake,
    backgroundAgentLimit,
    onBackgroundAgentLimit,
    terminalFontFamily,
    onTerminalFontFamily,
    terminalShell,
    terminalShellOptions,
    onTerminalShell,
    projectDirectory,
    onProjectDirectoryChoose,
    onProjectDirectoryReset,
    customInstructions,
    onCustomInstructionsSave,
    localMemories,
    onLocalMemoriesChange,
    memoryFile,
    onMemoryFileSave,
    onMemoriesReset,
    appUpdateDownloadSource,
    onAppUpdateDownloadSource,
    autoArchiveConversations,
    onAutoArchiveConversations,
    archiveRetentionDays,
    onArchiveRetentionDays,
  };
}

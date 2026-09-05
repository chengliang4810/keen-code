import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import type { SettingsRouteSettings } from "@/features/app/SettingsRoute";
import {
  persistLatestAppSetting,
  type AppSettingKey,
  type AppSettingPersistenceMap,
  type LatestAppSettingUpdate,
} from "@/lib/appSettingPersistence";

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
  /** 当前后端记忆状态；为空表示尚未加载或读取失败。 */
  const [memoryStatus, setMemoryStatus] = useState<api.MemoryStatus | null>(
    null,
  );
  /** 是否正在按需读取本机记忆状态。 */
  const [memoryStatusLoading, setMemoryStatusLoading] = useState(false);
  /** 最近一次读取本机记忆状态是否失败。 */
  const [memoryStatusError, setMemoryStatusError] = useState(false);
  /** 用于丢弃过期的记忆状态响应，避免快速操作后覆盖最新状态。 */
  const memoryStatusRequestSeqRef = useRef(0);

  const onSaveErrorRef = useRef(onSaveError);
  onSaveErrorRef.current = onSaveError;
  const showToastRef = useRef(showToast);
  showToastRef.current = showToast;
  const settingPersistenceRef = useRef<AppSettingPersistenceMap>(new Map());
  const tr = useMemo(() => createT(locale), [locale]);

  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  const reportSaveError = useCallback(() => {
    onSaveErrorRef.current?.(tr("settings.saveFailed"));
  }, [tr]);

  /** 所有单字段 AppSettings 保存都经过字段级修订、回滚与规范化出口。 */
  const updateSetting = useCallback(
    <Key extends AppSettingKey, State>(
      update: LatestAppSettingUpdate<Key, State>,
    ) => {
      void persistLatestAppSetting(update, {
        states: settingPersistenceRef.current,
        persist: api.settingsSet,
        onError: reportSaveError,
      });
    },
    [reportSaveError],
  );

  /** 按需刷新记忆状态；不建立常驻轮询，失败仅反映在状态卡片。 */
  const refreshMemoryStatus = useCallback(async () => {
    const requestSeq = ++memoryStatusRequestSeqRef.current;
    if (!api.isTauri()) {
      setMemoryStatus(null);
      setMemoryStatusError(false);
      setMemoryStatusLoading(false);
      return;
    }
    setMemoryStatusLoading(true);
    setMemoryStatusError(false);
    try {
      const status = await api.memoriesStatus();
      if (requestSeq !== memoryStatusRequestSeqRef.current) return;
      setMemoryStatus(status);
    } catch {
      if (requestSeq !== memoryStatusRequestSeqRef.current) return;
      // 刷新失败时保留最后一次成功快照，避免状态卡片因瞬时错误变空。
      setMemoryStatusError(true);
    } finally {
      if (requestSeq === memoryStatusRequestSeqRef.current) {
        setMemoryStatusLoading(false);
      }
    }
  }, []);

  // 从本地设置文件恢复常规选项；各资源独立加载，单项失败不阻塞其余设置。
  useEffect(() => {
    if (appBooting || !api.isTauri()) return;
    let active = true;
    void (async () => {
      try {
        const settings = await api.settingsGet();
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
      } catch {
        /* 单项设置读取失败时保留默认值，并继续读取其余资源。 */
      }
    })();
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
      memoryStatusRequestSeqRef.current += 1;
    };
  }, [appBooting]);

  const onLocaleChange = useCallback(
    (value: Locale) => {
      updateSetting({
        key: "interfaceLanguage",
        value,
        optimistic: value,
        previous: locale,
        apply: setLocale,
      });
    },
    [locale, updateSetting],
  );

  const onCustomInstructionsSave = useCallback(async (value: string) => {
    const saved = await api.customInstructionsSet(value);
    setCustomInstructions(saved);
  }, []);

  const onLocalMemoriesChange = useCallback(async (value: boolean) => {
    const saved = await api.settingsSet({ localMemories: value });
    setLocalMemories(saved.localMemories);
    await refreshMemoryStatus();
  }, [refreshMemoryStatus]);

  const onMemoryFileSave = useCallback(async (value: string) => {
    const saved = await api.memoriesSet(value);
    setMemoryFile(saved);
    await refreshMemoryStatus();
  }, [refreshMemoryStatus]);

  const onMemoriesReset = useCallback(async () => {
    await api.memoriesReset();
    setMemoryFile("");
    await refreshMemoryStatus();
    showToastRef.current(tr("settings.personalization.deleteMemoriesDone"));
  }, [refreshMemoryStatus, tr]);

  /** 在系统文件管理器中显示当前记忆根目录。 */
  const onRevealMemoryRoot = useCallback(async () => {
    const root = memoryStatus?.root.trim();
    if (!root || !api.isTauri()) return;
    await api.pathReveal(root);
  }, [memoryStatus?.root]);

  const onChromeHardwareAcceleration = useCallback(
    (value: boolean) => {
      updateSetting({
        key: "chromeHardwareAcceleration",
        value,
        optimistic: value,
        previous: chromeHardwareAcceleration,
        apply: setChromeHardwareAcceleration,
      });
    },
    [chromeHardwareAcceleration, updateSetting],
  );

  const onTaskNotifications = useCallback(
    (value: boolean) => {
      updateSetting({
        key: "taskNotifications",
        value,
        optimistic: value,
        previous: taskNotifications,
        apply: setTaskNotifications,
      });
    },
    [taskNotifications, updateSetting],
  );

  const onNotificationSound = useCallback(
    (value: boolean) => {
      updateSetting({
        key: "notificationSound",
        value,
        optimistic: value,
        previous: notificationSound,
        apply: setNotificationSound,
      });
    },
    [notificationSound, updateSetting],
  );

  const onAppUpdateDownloadSource = useCallback(
    (value: api.AppUpdateDownloadSource) => {
      updateSetting({
        key: "appUpdateDownloadSource",
        value,
        optimistic: value,
        previous: appUpdateDownloadSource,
        apply: setAppUpdateDownloadSource,
      });
    },
    [appUpdateDownloadSource, updateSetting],
  );

  const onKeepComputerAwake = useCallback(
    (value: boolean) => {
      updateSetting({
        key: "keepComputerAwake",
        value,
        optimistic: value,
        previous: keepComputerAwake,
        apply: setKeepComputerAwake,
      });
    },
    [keepComputerAwake, updateSetting],
  );

  const onBackgroundAgentLimit = useCallback(
    (value: number) => {
      updateSetting({
        key: "backgroundAgentLimit",
        value,
        optimistic: value,
        previous: backgroundAgentLimit,
        apply: setBackgroundAgentLimit,
        normalizeSaved: (saved) => saved.backgroundAgentLimit,
      });
    },
    [backgroundAgentLimit, updateSetting],
  );

  const onTerminalFontFamily = useCallback(
    (value: string) => {
      updateSetting({
        key: "terminalFontFamily",
        value,
        optimistic: value,
        previous: terminalFontFamily,
        apply: setTerminalFontFamily,
      });
    },
    [terminalFontFamily, updateSetting],
  );

  const onTerminalShell = useCallback(
    (value: api.TerminalShell) => {
      updateSetting({
        key: "terminalShell",
        value,
        optimistic: value,
        previous: terminalShell,
        apply: setTerminalShell,
      });
    },
    [terminalShell, updateSetting],
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
      updateSetting({
        key: "autoArchiveConversations",
        value,
        optimistic: value,
        previous: autoArchiveConversations,
        apply: setAutoArchiveConversations,
      });
    },
    [autoArchiveConversations, updateSetting],
  );

  const onArchiveRetentionDays = useCallback(
    (value: number) => {
      updateSetting({
        key: "archiveRetentionDays",
        value,
        optimistic: value,
        previous: archiveRetentionDays,
        apply: setArchiveRetentionDays,
      });
    },
    [archiveRetentionDays, updateSetting],
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
    memoryStatus,
    memoryStatusLoading,
    memoryStatusError,
    onRefreshMemoryStatus: refreshMemoryStatus,
    onRevealMemoryRoot,
    appUpdateDownloadSource,
    onAppUpdateDownloadSource,
    autoArchiveConversations,
    onAutoArchiveConversations,
    archiveRetentionDays,
    onArchiveRetentionDays,
  };
}

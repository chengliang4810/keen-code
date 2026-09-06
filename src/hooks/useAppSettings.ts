import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import type { SettingsRouteSettings } from "@/features/app/SettingsRoute";
import {
  createMemoryFileAccessState,
  refreshMemoryFile,
  writeMemoryFile,
} from "@/lib/memoryFileAccess";
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
  /** 进入个性化页时读取后台最新生成的记忆。 */
  onMemoryFileRefresh: () => Promise<void>;
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
  /** WebFetch 与 WebSearch 的兼容服务基础 URL；空值保持网络工具禁用。 */
  const [webServiceUrl, setWebServiceUrl] = useState("");
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
  const settingPersistenceRef = useRef<AppSettingPersistenceMap>(new Map());
  /** 统一启动读取、页面刷新、手动保存及重置的竞态边界。 */
  const memoryFileAccessRef = useRef(createMemoryFileAccessState());
  const tr = useMemo(() => createT(locale), [locale]);

  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  const reportSaveError = useCallback(() => {
    onSaveErrorRef.current?.(tr("settings.saveFailed"));
  }, [tr]);

  /** 刷新失败保持最后确认值，避免将读取故障展示为空记忆。 */
  const onMemoryFileRefresh = useCallback(async () => {
    if (!api.isTauri()) return;
    await refreshMemoryFile(
      memoryFileAccessRef.current,
      api.memoriesGet,
      setMemoryFile,
    ).catch(() => {});
  }, []);

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
        setWebServiceUrl(settings.webServiceUrl);
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
    void onMemoryFileRefresh();
    return () => {
      active = false;
      // 启动阶段切换或卸载后，不接受旧记忆读取回执。
      ++memoryFileAccessRef.current.revision;
    };
  }, [appBooting, onMemoryFileRefresh]);

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
  }, []);

  const onMemoryFileSave = useCallback(async (value: string) => {
    await writeMemoryFile(
      memoryFileAccessRef.current,
      () => api.memoriesSet(value),
      setMemoryFile,
    );
  }, []);

  const onMemoriesReset = useCallback(async () => {
    await writeMemoryFile(memoryFileAccessRef.current, async () => {
      await api.memoriesReset();
      return "";
    }, setMemoryFile);
    showToastRef.current(tr("settings.personalization.deleteMemoriesDone"));
  }, [tr]);

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

  /** 失焦后乐观保存兼容服务 URL，失败时由字段级持久化状态回滚。 */
  const onWebServiceUrl = useCallback(
    (value: string) => {
      updateSetting({
        key: "webServiceUrl",
        value,
        optimistic: value,
        previous: webServiceUrl,
        apply: setWebServiceUrl,
      });
    },
    [webServiceUrl, updateSetting],
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
    onMemoryFileRefresh,
    onMemoryFileSave,
    onMemoriesReset,
    appUpdateDownloadSource,
    onAppUpdateDownloadSource,
    autoArchiveConversations,
    onAutoArchiveConversations,
    archiveRetentionDays,
    onArchiveRetentionDays,
    webServiceUrl,
    onWebServiceUrl,
  };
}

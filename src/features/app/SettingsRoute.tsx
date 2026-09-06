import { lazy, Suspense, useEffect } from "react";
import type {
  AppUpdateDownloadSource,
  AppUpdateStatus,
  TerminalShell,
  TerminalShellOption,
} from "@/lib/api";
import type { Locale } from "@/i18n";
import type { ThemePreference } from "@/lib/theme";
import type {
  ThemeSkinId,
  WallpaperClip,
  WallpaperFocus,
  WallpaperKind,
  WallpaperRecord,
} from "@/lib/themeSkin";
import type { WallpaperFocusApplyResult } from "@/components/WallpaperFocusEditor";
import type {
  ArchivedSessionItem,
  SettingsPageProps,
  SettingsSectionId,
} from "@/components/SettingsPage";
import type { AppUpdateBusy } from "@/components/AppUpdateSection";

const SettingsPage = lazy(() =>
  import("@/components/SettingsPage").then((module) => ({
    default: module.SettingsPage,
  })),
);

const settingsPageFallback = (
  <div className="settings-page settings-page--fallback" aria-busy="true" />
);

/** 设置页导航由路由层负责，业务设置按域传入，避免 App 继续堆平铺 props。 */
export interface SettingsRouteNavigation {
  section: SettingsSectionId;
  onSection: (section: SettingsSectionId) => void;
  onBack: () => void;
}

/** 常规设置及其持久化动作。 */
export interface SettingsRouteSettings {
  locale: Locale;
  onLocaleChange: (locale: Locale) => void;
  chromeHardwareAcceleration: boolean;
  onChromeHardwareAcceleration?: (value: boolean) => void;
  taskNotifications: boolean;
  onTaskNotifications?: (value: boolean) => void;
  notificationSound: boolean;
  onNotificationSound?: (value: boolean) => void;
  keepComputerAwake: boolean;
  onKeepComputerAwake?: (value: boolean) => void;
  backgroundAgentLimit: number;
  onBackgroundAgentLimit: (value: number) => void;
  terminalFontFamily: string;
  onTerminalFontFamily: (value: string) => void;
  terminalShell: TerminalShell;
  terminalShellOptions: readonly TerminalShellOption[];
  onTerminalShell: (value: TerminalShell) => void;
  projectDirectory: string;
  onProjectDirectoryChoose: () => Promise<void>;
  onProjectDirectoryReset: () => Promise<void>;
  customInstructions: string;
  onCustomInstructionsSave: (value: string) => Promise<void>;
  localMemories: boolean;
  onLocalMemoriesChange: (value: boolean) => Promise<void>;
  memoryFile: string;
  onMemoryFileSave: (value: string) => Promise<void>;
  onMemoriesReset: () => Promise<void>;
  /** WebFetch 与 WebSearch 使用的兼容服务基础 URL；为空时禁用网络工具。 */
  webServiceUrl: string;
  /** 持久化兼容服务基础 URL；空字符串用于关闭网络工具。 */
  onWebServiceUrl: (value: string) => void;
}

/** 设置页中依赖当前工作区/供应商状态的会话上下文。 */
export interface SettingsRouteSession {
  projectPath?: string | null;
  onProviderActivated?: () => void;
}

/** 自动归档配置和已归档会话操作。 */
export interface SettingsRouteArchive {
  /** null 表示设置尚未从本地恢复；路由显示当前默认值。 */
  autoArchiveConversations: boolean | null;
  onAutoArchiveConversations: (value: boolean) => void;
  archiveRetentionDays: number;
  onArchiveRetentionDays: (value: number) => void;
  archivedSessions?: readonly ArchivedSessionItem[];
  onRestoreArchivedSession?: (
    sessionId: string,
  ) => void | Promise<void>;
  onDeleteArchivedSession?: (sessionId: string) => void;
}

/** 外观状态与壁纸资源操作。 */
export interface SettingsRouteAppearance {
  themePreference: ThemePreference;
  onTheme: (value: ThemePreference) => void;
  skin: ThemeSkinId;
  onSkin: (value: ThemeSkinId) => void;
  wallpaperUrl?: string | null;
  wallpaperKind?: WallpaperKind | null;
  wallpaperFocus?: WallpaperFocus | null;
  wallpaperClip?: WallpaperClip | null;
  wallpaperMediaSize?: { w: number; h: number } | null;
  onWallpaper?: (record: WallpaperRecord | null) => void | Promise<void>;
  onWallpaperAdjust?: (result: WallpaperFocusApplyResult) => void;
  onWallpaperMediaSize?: (size: { w: number; h: number }) => void;
  wallpaperScrim?: number;
  onWallpaperScrim?: (value: number) => void;
  wallpaperBlur?: number;
  onWallpaperBlur?: (value: number) => void;
  onWallpaperAppearanceReset?: () => void;
}

/** 更新信息与手动检查/安装动作。 */
export interface SettingsRouteUpdate {
  versionFooter: string;
  appUpdateStatus: AppUpdateStatus | null;
  appUpdateBusy: AppUpdateBusy;
  appUpdateError: string | null;
  appUpdateDownloadSource: AppUpdateDownloadSource;
  onAppUpdateDownloadSource: (value: AppUpdateDownloadSource) => void;
  onAppUpdateCheck: () => void | Promise<void>;
  onAppUpdateInstall: () => void | Promise<void>;
}

export interface SettingsRouteProps extends SettingsRouteNavigation {
  /** 路由重新进入个性化页时刷新，不向展示组件下传副作用。 */
  onMemoryFileRefresh: () => Promise<void>;
  settings: SettingsRouteSettings;
  session: SettingsRouteSession;
  archive: SettingsRouteArchive;
  appearance: SettingsRouteAppearance;
  update: SettingsRouteUpdate;
}

/** 设置路由适配器：只在这里把按域 props 映射为 SettingsPage 的兼容平面。 */
export function SettingsRoute({
  section,
  onSection,
  onBack,
  onMemoryFileRefresh,
  settings,
  session,
  archive,
  appearance,
  update,
}: SettingsRouteProps) {
  // 只在进入个性化分区时读取；不因正文或草稿变化重复请求。
  useEffect(() => {
    if (section === "personalization") void onMemoryFileRefresh();
  }, [section, onMemoryFileRefresh]);

  const pageProps: SettingsPageProps = {
    section,
    onSection,
    onBack,
    ...settings,
    ...session,
    ...appearance,
    ...update,
    autoArchiveConversations: archive.autoArchiveConversations ?? true,
    onAutoArchiveConversations: archive.onAutoArchiveConversations,
    archiveRetentionDays: archive.archiveRetentionDays,
    onArchiveRetentionDays: archive.onArchiveRetentionDays,
    archivedSessions: archive.archivedSessions,
    onRestoreArchivedSession: archive.onRestoreArchivedSession,
    onDeleteArchivedSession: archive.onDeleteArchivedSession,
  };

  return (
    <Suspense fallback={settingsPageFallback}>
      <SettingsPage {...pageProps} />
    </Suspense>
  );
}

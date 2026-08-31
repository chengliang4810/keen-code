import { Label } from "@/components/ui/label";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Slider } from "@/components/ui/slider";
/**
 * Full-page settings shell (ChatGPT-desktop style): left nav + content.
 * Back control returns to the workbench ("返回应用").
 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  IconActivity,
  IconAppearance,
  IconArrowLeft,
  IconArchive,
  IconCrop,
  IconExternalLink,
  IconInfo,
  IconList,
  IconPlug,
  IconPuzzle,
  IconSettings,
  IconSkills,
  IconSubagent,
  IconSummary,
  IconUser,
} from "@/components/icons";
import { isTauri, urlOpen } from "@/lib/api";
import {
  isThemePreference,
  type ThemePreference,
} from "@/lib/theme";
import {
  DEFAULT_WALLPAPER_FOCUS,
  THEME_SKINS,
  WALLPAPER_ACCEPT,
  WallpaperPrepareError,
  isThemeSkinId,
  prepareWallpaperFromFile,
  type ThemeSkinId,
  type WallpaperClip,
  type WallpaperFocus,
  type WallpaperKind,
  type WallpaperRecord,
} from "@/lib/themeSkin";
import {
  WallpaperFocusEditor,
  type WallpaperFocusApplyResult,
} from "@/components/WallpaperFocusEditor";
import { WallpaperMediaLayer } from "@/components/WallpaperMediaLayer";
import { ProvidersPanel } from "@/components/ProvidersPanel";
import { ExtensionsPanel } from "@/components/ExtensionsPanel";
import { AgentsPanel } from "@/components/AgentsPanel";
import { AnalyticsSettingsPanel } from "@/components/AnalyticsSettingsPanel";
import { RequestHistoryPanel } from "@/components/RequestHistoryPanel";
import { PersonalizationSettingsPanel } from "@/components/PersonalizationSettingsPanel";
import {
  AppUpdateSection,
  type AppUpdateBusy,
} from "@/components/AppUpdateSection";
import type {
  AppUpdateDownloadSource,
  AppUpdateStatus,
  TerminalShell,
  TerminalShellOption,
} from "@/lib/api";
import {
  createT,
  isLocale,
  type Locale,
  type MessageKey,
  type Vars,
} from "@/i18n";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  ToggleGroup,
  ToggleGroupItem,
} from "@/components/ui/toggle-group";
import {
  SETTINGS_NAV,
  SETTINGS_NAV_GROUPS,
  buildSettingsHash,
  getNavDef,
  isSettingsSectionId,
  type SettingsNavIcon,
  type SettingsSectionId,
} from "@/lib/settingsCatalog";

export type { SettingsSectionId } from "@/lib/settingsCatalog";

const SOURCE_REPOSITORY_URL = "https://github.com/chengliang4810/keen-code";
const MIN_BACKGROUND_AGENT_LIMIT = 1;
const MAX_BACKGROUND_AGENT_LIMIT = 999;

export interface SettingsPageProps {
  section: SettingsSectionId;
  /** 跳转到一个当前设置分区。 */
  onSection: (id: SettingsSectionId) => void;
  onBack: () => void;
  locale: Locale;
  /** 保存并立即应用界面语言。 */
  onLocaleChange: (locale: Locale) => void;
  /** 用户选择的主题偏好，包含跟随系统。 */
  themePreference: ThemePreference;
  onTheme: (v: ThemePreference) => void;
  /** 叠加在明暗主题上的颜色皮肤。 */
  skin: ThemeSkinId;
  /** 应用颜色皮肤。 */
  onSkin: (v: ThemeSkinId) => void;
  /** Custom wallpaper blob: URL (null/undefined = none). */
  wallpaperUrl?: string | null;
  /** Kind of the current wallpaper, to pick <video> vs <img> in the preview. */
  wallpaperKind?: WallpaperKind | null;
  /** Pan/zoom focus for the wallpaper (window-aspect crop). */
  wallpaperFocus?: WallpaperFocus | null;
  /** Video in/out clip (seconds). */
  wallpaperClip?: WallpaperClip | null;
  /** Intrinsic media size from meta (avoids video preview flash). */
  wallpaperMediaSize?: { w: number; h: number } | null;
  onWallpaper?: (record: WallpaperRecord | null) => void | Promise<void>;
  /** Save focus crop + optional video clip (no blob rewrite). */
  onWallpaperAdjust?: (result: WallpaperFocusApplyResult) => void;
  /** 首次解码成功后保存媒体固有尺寸。 */
  onWallpaperMediaSize?: (size: { w: number; h: number }) => void;
  /** Wallpaper scrim strength 0–100 (only the dimming overlay; not chrome). */
  wallpaperScrim?: number;
  onWallpaperScrim?: (value: number) => void;
  /** Wallpaper blur radius in CSS pixels. */
  wallpaperBlur?: number;
  onWallpaperBlur?: (value: number) => void;
  onWallpaperAppearanceReset?: () => void;
  /** Windows WebView2 是否启用硬件加速。 */
  chromeHardwareAcceleration?: boolean;
  onChromeHardwareAcceleration?: (v: boolean) => void;
  /** 是否发送任务完成、失败和等待确认的桌面通知。 */
  taskNotifications?: boolean;
  onTaskNotifications?: (v: boolean) => void;
  /** 任务通知是否播放系统默认提示音。 */
  notificationSound?: boolean;
  onNotificationSound?: (v: boolean) => void;
  /** 是否阻止系统因用户空闲自动进入睡眠。 */
  keepComputerAwake?: boolean;
  onKeepComputerAwake?: (v: boolean) => void;
  /** 每个会话允许同时运行的后台 Agent 数量。 */
  backgroundAgentLimit: number;
  onBackgroundAgentLimit: (value: number) => void;
  /** 内置终端使用的 CSS 字体族列表。 */
  terminalFontFamily: string;
  onTerminalFontFamily: (value: string) => void;
  terminalShell: TerminalShell;
  terminalShellOptions: readonly TerminalShellOption[];
  onTerminalShell: (value: TerminalShell) => void;
  /** 新项目未选择现有目录时使用的默认父目录。 */
  projectDirectory: string;
  onProjectDirectoryChoose: () => Promise<void>;
  onProjectDirectoryReset: () => Promise<void>;
  autoArchiveConversations: boolean;
  onAutoArchiveConversations: (v: boolean) => void;
  archiveRetentionDays: number;
  onArchiveRetentionDays: (v: number) => void;
  /** 当前持久化的已归档对话。 */
  archivedSessions?: readonly ArchivedSessionItem[];
  /** 将指定对话恢复到工作台。 */
  onRestoreArchivedSession?: (sessionId: string) => void | Promise<void>;
  /** 请求永久删除指定的已归档对话。 */
  onDeleteArchivedSession?: (sessionId: string) => void;
  versionFooter: string;
  /** 当前构建版本与最近一次更新检查结果。 */
  appUpdateStatus: AppUpdateStatus | null;
  /** 当前更新操作。 */
  appUpdateBusy: AppUpdateBusy;
  /** 手动检查或安装的最近错误。 */
  appUpdateError: string | null;
  appUpdateDownloadSource: AppUpdateDownloadSource;
  onAppUpdateDownloadSource: (value: AppUpdateDownloadSource) => void;
  onAppUpdateCheck: () => void | Promise<void>;
  onAppUpdateInstall: () => void | Promise<void>;
  /** 自定义供应商切换后刷新桌面端展示状态。 */
  onProviderActivated?: () => void;
  /** Active project path for Skills/MCP inspect cwd. */
  projectPath?: string | null;
  /** 最近一次成功持久化的全局自定义指令。 */
  customInstructions: string;
  /** 保存全局自定义指令；失败时应 reject 并由面板保留草稿。 */
  onCustomInstructionsSave: (value: string) => Promise<void>;
  localMemories: boolean;
  onLocalMemoriesChange: (value: boolean) => Promise<void>;
  /** 最近一次成功持久化的长期记忆正文。 */
  memoryFile: string;
  /** 保存长期记忆正文；失败时应 reject 并由面板保留草稿。 */
  onMemoryFileSave: (value: string) => Promise<void>;
  onMemoriesReset: () => Promise<void>;
}

/** 设置页展示归档对话所需的最小投影。 */
export interface ArchivedSessionItem {
  /** 对话唯一标识。 */
  id: string;
  /** 对话标题。 */
  title: string;
  /** 所属项目名称；无项目时为空。 */
  projectName: string | null;
  /** 最后更新时间。 */
  updatedAt: string;
}

function NavIcon({
  name,
  size = 18,
}: {
  name: SettingsNavIcon;
  size?: number;
}) {
  if (name === "appearance") return <IconAppearance size={size} />;
  if (name === "archive") return <IconArchive size={size} />;
  if (name === "user") return <IconUser size={size} />;
  if (name === "extensions") return <IconPuzzle size={size} />;
  if (name === "skills") return <IconSkills size={size} />;
  if (name === "agents") return <IconSubagent size={size} />;
  if (name === "mcp") return <IconPlug size={size} />;
  if (name === "requests") return <IconList size={size} />;
  if (name === "info") return <IconInfo size={size} />;
  if (name === "personalization") return <IconSummary size={size} />;
  if (name === "analytics") return <IconActivity size={size} />;
  return <IconSettings size={size} />;
}

/** 设置页布尔选项使用的开关控件。 */
function SettingsSwitch({
  checked,
  disabled = false,
  onChange,
  ariaLabel,
}: {
  /** 当前是否开启。 */
  checked: boolean;
  /** 当前是否禁止操作。 */
  disabled?: boolean;
  /** 开关状态变化回调。 */
  onChange: (checked: boolean) => void;
  /** 辅助功能标签。 */
  ariaLabel: string;
}) {
  return (
    <Switch
      type="button"
      checked={checked}
      aria-label={ariaLabel}
      title={ariaLabel}
      disabled={disabled}
      className={"ext-switch" + (checked ? " is-on" : "")}
      onCheckedChange={(value) => onChange(value === true)}
      onClick={(event) => event.stopPropagation()}
      onPointerDown={(event) => event.stopPropagation()}
    />
  );
}

export function SettingsPage({
  section,
  onSection,
  onBack,
  locale,
  onLocaleChange,
  themePreference,
  onTheme,
  skin,
  onSkin,
  wallpaperUrl = null,
  wallpaperKind = null,
  wallpaperFocus = null,
  wallpaperClip = null,
  wallpaperMediaSize = null,
  wallpaperScrim = 40,
  onWallpaperScrim,
  wallpaperBlur = 0,
  onWallpaperBlur,
  onWallpaperAppearanceReset,
  onWallpaper,
  onWallpaperAdjust,
  onWallpaperMediaSize,
  chromeHardwareAcceleration = true,
  onChromeHardwareAcceleration,
  taskNotifications = true,
  onTaskNotifications,
  notificationSound = true,
  onNotificationSound,
  keepComputerAwake = true,
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
  autoArchiveConversations,
  onAutoArchiveConversations,
  archiveRetentionDays,
  onArchiveRetentionDays,
  archivedSessions = [],
  onRestoreArchivedSession,
  onDeleteArchivedSession,
  versionFooter,
  appUpdateStatus,
  appUpdateBusy,
  appUpdateError,
  appUpdateDownloadSource,
  onAppUpdateDownloadSource,
  onAppUpdateCheck,
  onAppUpdateInstall,
  onProviderActivated,
  projectPath = null,
  customInstructions,
  onCustomInstructionsSave,
  localMemories,
  onLocalMemoriesChange,
  memoryFile,
  onMemoryFileSave,
  onMemoriesReset,
}: SettingsPageProps) {
  const titleRef = useRef<HTMLHeadingElement>(null);
  const previousSectionRef = useRef(section);
  const wallpaperInputRef = useRef<HTMLInputElement>(null);
  const [wallpaperBusy, setWallpaperBusy] = useState(false);
  const [wallpaperError, setWallpaperError] = useState<string | null>(null);
  const [wallpaperFocusOpen, setWallpaperFocusOpen] = useState(false);
  /** 已归档对话的本地查询词。 */
  const [archivedQuery, setArchivedQuery] = useState("");
  /** 正在恢复的对话标识，避免重复提交。 */
  const [restoringSessionId, setRestoringSessionId] = useState<string | null>(null);
  /** 设置页直接使用完整语言目录。 */
  const tr = useMemo(() => createT(locale), [locale]);
  const t = useCallback(
    (k: string, vars?: Vars) => tr(k as MessageKey, vars),
    [tr],
  );
  const wallpaperErrorMessage = useCallback(
    (err: unknown): string => {
      if (err instanceof WallpaperPrepareError) {
        const key = `settings.wallpaper.err.${err.code}` as MessageKey;
        const msg = t(key);
        return msg === key ? t("settings.wallpaper.err.generic") : msg;
      }
      return t("settings.wallpaper.err.generic");
    },
    [t],
  );

  const onWallpaperFile = useCallback(
    async (file: File | null | undefined) => {
      if (!file || !onWallpaper) return;
      setWallpaperBusy(true);
      setWallpaperError(null);
      try {
        const record = await prepareWallpaperFromFile(file);
        await onWallpaper(record);
      } catch (e) {
        setWallpaperError(wallpaperErrorMessage(e));
      } finally {
        setWallpaperBusy(false);
        if (wallpaperInputRef.current) wallpaperInputRef.current.value = "";
      }
    },
    [onWallpaper, wallpaperErrorMessage],
  );

  /** 跳转当前设置分区并同步唯一 Hash。 */
  const navigateTo = useCallback(
    (id: SettingsSectionId) => {
      onSection(id);
      if (typeof window !== "undefined") {
        const hash = buildSettingsHash({ section: id });
        if (window.location.hash !== hash) {
          window.location.hash = hash;
        }
      }
    },
    [onSection],
  );

  /** 打开一个设置分区。 */
  const openSection = useCallback(
    (id: SettingsSectionId) => {
      navigateTo(id);
    },
    [navigateTo],
  );

  const nav = SETTINGS_NAV;

  const navGroups = useMemo(
    () =>
      SETTINGS_NAV_GROUPS.map((group) => ({
        ...group,
        items: nav.filter((item) => item.group === group.id),
      })),
    [nav],
  );
  const standaloneNav = useMemo(
    () => nav.filter((item) => item.group === null),
    [nav],
  );

  const sectionNav = getNavDef(section);
  if (!sectionNav) {
    throw new Error(`未注册的设置分区：${section}`);
  }
  const title = t(sectionNav.labelKey);
  useEffect(() => {
    const previousTitle = document.title;
    document.title = `${title} · KeenCode`;
    if (previousSectionRef.current !== section) {
      titleRef.current?.focus({ preventScroll: true });
      previousSectionRef.current = section;
    }
    return () => {
      document.title = previousTitle;
    };
  }, [section, title]);
  /** 按标题或项目名称过滤已归档对话。 */
  const visibleArchivedSessions = useMemo(() => {
    const query = archivedQuery.trim().toLocaleLowerCase(locale);
    if (!query) return archivedSessions;
    return archivedSessions.filter((session) =>
      `${session.title} ${session.projectName ?? ""}`
        .toLocaleLowerCase(locale)
        .includes(query),
    );
  }, [archivedQuery, archivedSessions, locale]);

  /** 恢复单个对话并锁定对应按钮。 */
  const restoreArchivedSession = useCallback(async (sessionId: string) => {
    if (!onRestoreArchivedSession || restoringSessionId) return;
    setRestoringSessionId(sessionId);
    try {
      await onRestoreArchivedSession(sessionId);
    } finally {
      setRestoringSessionId(null);
    }
  }, [onRestoreArchivedSession, restoringSessionId]);

  const renderNavItem = (n: (typeof SETTINGS_NAV)[number]) => (
    <a
      key={n.id}
      href={buildSettingsHash({ section: n.id })}
      aria-current={section === n.id ? "page" : undefined}
      className={
        "settings-page__nav-item" +
        (section === n.id ? " is-active" : "")
      }
      onClick={(event) => {
        event.preventDefault();
        openSection(n.id);
      }}
    >
      <NavIcon name={n.icon} size={18} />
      <span className="settings-page__nav-label">{t(n.labelKey)}</span>
    </a>
  );

  return (
    <div className="settings-page" data-testid="settings-page">
      {/* Full-width overlay drag band (does not break glass nav continuity) */}
      <div
        className="settings-page__chrome"
        data-tauri-drag-region
        aria-hidden
        onDoubleClick={() => {
          void import("@tauri-apps/api/window")
            .then(({ getCurrentWindow }) => getCurrentWindow().toggleMaximize())
            .catch(() => {});
        }}
      />
      <a className="settings-page__skip-link" href="#settings-main">
        {t("settings.skipToContent")}
      </a>
      <aside className="settings-page__nav">
        <div className="settings-page__mobile-nav">
          <Button
            type="button"
            className="settings-page__mobile-back"
            onClick={onBack}
          >
            <IconArrowLeft size={18} />
            <span>{t("settings.backToApp")}</span>
          </Button>
          <Select
            value={section}
            onValueChange={(value) => {
              if (isSettingsSectionId(value)) openSection(value);
            }}
          >
            <SelectTrigger
              className="settings-input settings-page__mobile-select"
              aria-label={t("settings.navigation")}
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {navGroups.map((group) => (
                <SelectGroup key={group.id}>
                  <SelectLabel>{t(group.labelKey)}</SelectLabel>
                  {group.items.map((item) => (
                    <SelectItem key={item.id} value={item.id}>
                      {t(item.labelKey)}
                    </SelectItem>
                  ))}
                </SelectGroup>
              ))}
              {standaloneNav.map((item) => (
                <SelectItem key={item.id} value={item.id}>
                  {t(item.labelKey)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <nav
          className="settings-page__nav-inner"
          aria-label={t("settings.navigation")}
        >
          <Button
            type="button"
            className="settings-page__back"
            onClick={onBack}
          >
            <IconArrowLeft size={16} />
            <span>{t("settings.backToApp")}</span>
          </Button>


          {navGroups.map((group) =>
            group.items.length > 0 ? (
              <div
                className="settings-page__nav-group"
                key={group.id}
                role="group"
                aria-labelledby={`settings-nav-group-${group.id}`}
              >
                <div
                  className="settings-page__group-label"
                  id={`settings-nav-group-${group.id}`}
                >
                  {t(group.labelKey)}
                </div>
                {group.items.map(renderNavItem)}
              </div>
            ) : null,
          )}
          {standaloneNav.map(renderNavItem)}
        </nav>
      </aside>

      <div className="settings-page__content">
        <main className="settings-page__main" id="settings-main" tabIndex={-1}>
          <div className="settings-page__heading">
            <span className="settings-page__title-icon" aria-hidden="true">
              <NavIcon name={sectionNav.icon} size={20} />
            </span>
            <h1 className="settings-page__title" ref={titleRef} tabIndex={-1}>
              {title}
            </h1>
          </div>

          {section === "general" && (
            <>
              <h2 className="settings-page__h2">
                {t("settings.general.system")}
              </h2>
              <div className="settings-card">
                <div
                  className="settings-row"
                  id="settings-anchor-interface-language"
                >
                  <div className="settings-row__text">
                    <div className="settings-row__label">
                      {t("settings.interfaceLanguage")}
                    </div>
                    <div className="settings-row__desc">
                      {t("settings.interfaceLanguageDesc")}
                    </div>
                  </div>
                  <Select
                    value={locale}
                    onValueChange={(value) => {
                      if (isLocale(value)) onLocaleChange(value);
                    }}
                  >
                    <SelectTrigger
                      className="settings-input settings-input--compact"
                      aria-label={t("settings.interfaceLanguage")}
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectGroup>
                        <SelectItem value="zh">简体中文</SelectItem>
                        <SelectItem value="zh-TW">繁體中文</SelectItem>
                        <SelectItem value="en">English</SelectItem>
                      </SelectGroup>
                    </SelectContent>
                  </Select>
                </div>
                {onChromeHardwareAcceleration ? (
                  <div
                    className="settings-row"
                    id="settings-anchor-hardware-acceleration"
                  >
                    <div className="settings-row__text">
                      <div className="settings-row__label">
                        {t("settings.chromeHardwareAcceleration")}
                      </div>
                      <div className="settings-row__desc">
                        {t("settings.chromeHardwareAccelerationDesc")}
                      </div>
                    </div>
                    <SettingsSwitch
                      checked={chromeHardwareAcceleration}
                      onChange={onChromeHardwareAcceleration}
                      ariaLabel={t("settings.chromeHardwareAcceleration")}
                    />
                  </div>
                ) : null}
                <div className="settings-row" id="settings-anchor-keep-awake">
                  <div className="settings-row__text">
                    <div className="settings-row__label">
                      {t("settings.keepComputerAwake")}
                    </div>
                    <div className="settings-row__desc">
                      {t("settings.keepComputerAwakeDesc")}
                    </div>
                  </div>
                  <SettingsSwitch
                    checked={keepComputerAwake}
                    onChange={(checked) => onKeepComputerAwake?.(checked)}
                    ariaLabel={t("settings.keepComputerAwake")}
                  />
                </div>
                <div
                  className="settings-row"
                  id="settings-anchor-background-agent-limit"
                >
                  <div className="settings-row__text">
                    <div className="settings-row__label">
                      {t("settings.backgroundAgentLimit")}
                    </div>
                    <div
                      className="settings-row__desc"
                      id="settings-background-agent-limit-desc"
                    >
                      {t("settings.backgroundAgentLimitDesc")}
                    </div>
                  </div>
                  <Input
                    key={backgroundAgentLimit}
                    type="number"
                    className="settings-input settings-input--compact"
                    min={MIN_BACKGROUND_AGENT_LIMIT}
                    max={MAX_BACKGROUND_AGENT_LIMIT}
                    step={1}
                    defaultValue={backgroundAgentLimit}
                    aria-label={t("settings.backgroundAgentLimit")}
                    aria-describedby="settings-background-agent-limit-desc"
                    onBlur={(event) => {
                      const value = event.currentTarget.valueAsNumber;
                      if (
                        !Number.isInteger(value) ||
                        value < MIN_BACKGROUND_AGENT_LIMIT ||
                        value > MAX_BACKGROUND_AGENT_LIMIT
                      ) {
                        event.currentTarget.value = String(backgroundAgentLimit);
                        return;
                      }
                      if (value !== backgroundAgentLimit) {
                        onBackgroundAgentLimit(value);
                      }
                    }}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") event.currentTarget.blur();
                    }}
                  />
                </div>
                <div
                  className="settings-row settings-row--stack"
                  id="settings-anchor-project-directory"
                >
                  <div className="settings-row__text">
                    <div className="settings-row__label">
                      {t("settings.projectDirectory")}
                    </div>
                  </div>
                  <div className="settings-project-directory">
                    <code className="settings-row__hint" title={projectDirectory}>
                      {projectDirectory}
                    </code>
                    <div className="settings-project-directory__actions">
                      <Button
                        type="button"
                        className="btn btn--ghost btn--sm"
                        onClick={() => void onProjectDirectoryReset()}
                      >
                        {t("settings.projectDirectoryReset")}
                      </Button>
                      <Button
                        type="button"
                        className="btn btn--solid btn--sm"
                        onClick={() => void onProjectDirectoryChoose()}
                      >
                        {t("settings.projectDirectoryChoose")}
                      </Button>
                    </div>
                  </div>
                </div>
              </div>

              <h2 className="settings-page__h2">
                {t("settings.general.notifications")}
              </h2>
              <div className="settings-card">
                <div
                  className="settings-row"
                  id="settings-anchor-task-notifications"
                >
                  <div className="settings-row__text">
                    <div className="settings-row__label">
                      {t("settings.taskNotifications")}
                    </div>
                    <div className="settings-row__desc">
                      {t("settings.taskNotificationsDesc")}
                    </div>
                  </div>
                  <SettingsSwitch
                    checked={taskNotifications}
                    onChange={(checked) => onTaskNotifications?.(checked)}
                    ariaLabel={t("settings.taskNotifications")}
                  />
                </div>
                <div
                  className={
                    "settings-row" + (!taskNotifications ? " is-disabled" : "")
                  }
                  id="settings-anchor-notification-sound"
                >
                  <div className="settings-row__text">
                    <div className="settings-row__label">
                      {t("settings.notificationSound")}
                    </div>
                    <div className="settings-row__desc">
                      {t("settings.notificationSoundDesc")}
                    </div>
                  </div>
                  <SettingsSwitch
                    checked={notificationSound}
                    disabled={!taskNotifications}
                    onChange={(checked) => onNotificationSound?.(checked)}
                    ariaLabel={t("settings.notificationSound")}
                  />
                </div>
              </div>

            </>
          )}

          {section === "archive" && (
            <div className="settings-card">
              <div className="settings-row" id="settings-anchor-auto-archive">
                <div className="settings-row__text">
                  <div className="settings-row__label">{t("settings.archive.auto")}</div>
                  <div className="settings-row__desc">{t("settings.archive.autoDesc")}</div>
                </div>
                <SettingsSwitch
                  checked={autoArchiveConversations}
                  onChange={onAutoArchiveConversations}
                  ariaLabel={t("settings.archive.auto")}
                />
              </div>
              <div className={"settings-row" + (!autoArchiveConversations ? " is-disabled" : "")}>
                <div className="settings-row__text">
                  <div className="settings-row__label">{t("settings.archive.retention")}</div>
                  <div className="settings-row__desc">{t("settings.archive.retentionDesc")}</div>
                </div>
                <Select
                  value={String(archiveRetentionDays)}
                  disabled={!autoArchiveConversations}
                  onValueChange={(value) => onArchiveRetentionDays(Number(value))}
                >
                  <SelectTrigger className="settings-input settings-input--compact" aria-label={t("settings.archive.retention")}>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {[1, 3, 7, 14, 30, 60, 90].map((days) => (
                      <SelectItem key={days} value={String(days)}>
                        {t("settings.archive.afterDays", { days })}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
          )}

        {section === "archived" && (
          <>
            <p className="settings-page__lead">{t("settings.archived.desc")}</p>
            <Input
              id="settings-anchor-archived-conversations"
              type="search"
              className="settings-input settings-archived__search"
              value={archivedQuery}
              placeholder={t("settings.archived.search")}
              aria-label={t("settings.archived.search")}
              onChange={(event) => setArchivedQuery(event.target.value)}
            />
            <div className="settings-card settings-archived__list">
              {visibleArchivedSessions.length === 0 ? (
                <div className="settings-archived__empty">
                  {archivedSessions.length === 0
                    ? t("settings.archived.empty")
                    : t("settings.archived.noMatches")}
                </div>
              ) : visibleArchivedSessions.map((archivedSession) => (
                <div className="settings-row" key={archivedSession.id}>
                  <div className="settings-row__text">
                    <div className="settings-row__label">{archivedSession.title}</div>
                    <div className="settings-row__desc">
                      {archivedSession.projectName ?? t("settings.archived.noProject")}
                      {" · "}
                      {new Intl.DateTimeFormat(locale, {
                        dateStyle: "medium",
                        timeStyle: "short",
                      }).format(new Date(archivedSession.updatedAt))}
                    </div>
                  </div>
                  <div className="settings-archived__actions">
                    <Button
                      type="button"
                      className="btn btn--solid btn--sm"
                      disabled={restoringSessionId !== null}
                      onClick={() => void restoreArchivedSession(archivedSession.id)}
                    >
                      {restoringSessionId === archivedSession.id
                        ? t("settings.archived.restoring")
                        : t("settings.archived.restore")}
                    </Button>
                    <Button
                      type="button"
                      className="btn btn--danger btn--sm"
                      disabled={restoringSessionId !== null}
                      onClick={() => onDeleteArchivedSession?.(archivedSession.id)}
                    >
                      {t("settings.archived.delete")}
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          </>
        )}

        {section === "appearance" && (
          <>
            <div className="settings-card" id="settings-anchor-theme">
              <div className="settings-row">
                <div className="settings-row__text">
                  <div className="settings-row__label">
                    <IconAppearance size={16} />
                    {t("settings.theme")}
                  </div>
                  <div className="settings-row__desc">
                    {t("settings.themeDesc")}
                  </div>
                </div>
                <ToggleGroup
                  type="single"
                  value={themePreference}
                  aria-label={t("settings.theme")}
                  className="settings-seg"
                  onValueChange={(value) => {
                    if (isThemePreference(value)) onTheme(value);
                  }}
                >
                  <ToggleGroupItem value="system" className="settings-seg__btn">
                    {t("settings.themeSystem")}
                  </ToggleGroupItem>
                  <ToggleGroupItem value="light" className="settings-seg__btn">
                    {t("settings.themeLight")}
                  </ToggleGroupItem>
                  <ToggleGroupItem value="dark" className="settings-seg__btn">
                    {t("settings.themeDark")}
                  </ToggleGroupItem>
                </ToggleGroup>
              </div>
            </div>
            <div className="settings-card" id="settings-anchor-terminal-font">
              <div className="settings-row settings-row--stack">
                <div className="settings-row__text">
                  <Label className="settings-row__label" htmlFor="settings-terminal-font">
                    {t("settings.terminalFont")}
                  </Label>
                  <div className="settings-row__desc" id="settings-terminal-font-desc">
                    {t("settings.terminalFontDesc")}
                  </div>
                </div>
                <Input
                  key={terminalFontFamily}
                  id="settings-terminal-font"
                  className="settings-input"
                  defaultValue={terminalFontFamily}
                  maxLength={256}
                  aria-describedby="settings-terminal-font-desc"
                  onBlur={(event) => {
                    const value = event.currentTarget.value.trim();
                    if (!value) {
                      event.currentTarget.value = terminalFontFamily;
                      return;
                    }
                    if (value !== terminalFontFamily) onTerminalFontFamily(value);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") event.currentTarget.blur();
                  }}
                />
              </div>
            </div>
            {terminalShellOptions.length > 0 && (
              <div className="settings-card">
                <div className="settings-row">
                  <div className="settings-row__text">
                    <div className="settings-row__label">
                      {t("settings.terminalShell")}
                    </div>
                    <div className="settings-row__desc">
                      {t("settings.terminalShellDesc")}
                    </div>
                  </div>
                  <Select
                    value={terminalShell}
                    onValueChange={(value) =>
                      onTerminalShell(value as TerminalShell)
                    }
                  >
                    <SelectTrigger
                      className="settings-input settings-input--compact"
                      aria-label={t("settings.terminalShell")}
                    >
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="auto">
                        {t("settings.terminalShellAuto")}
                      </SelectItem>
                      {terminalShellOptions.map((shell) => (
                        <SelectItem key={shell.id} value={shell.id}>
                          {shell.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
              </div>
            )}
            <div className="settings-appearance-duo">
              <div
                className="settings-card settings-card--appearance-col"
                id="settings-anchor-skin"
              >
                <div className="settings-row settings-row--stack">
                  <div className="settings-row__text">
                    <div className="settings-row__label">
                      {t("settings.skin")}
                    </div>
                    <div className="settings-row__desc">
                      {t("settings.skinDesc")}
                    </div>
                  </div>
                  <RadioGroup
                    value={skin}
                    aria-label={t("settings.skin")}
                    className="settings-skin-grid"
                    onValueChange={(value) => {
                      if (isThemeSkinId(value)) onSkin(value);
                    }}
                  >
                    {THEME_SKINS.map((pack) => {
                      const label = t(
                        `settings.skin.${pack.id}` as "settings.skin.default",
                      );
                      return (
                        <RadioGroupItem
                          key={pack.id}
                          value={pack.id}
                          className="settings-skin-card"
                        >
                          <span
                            className="settings-skin-card__swatch"
                            style={{
                              background: `linear-gradient(135deg, ${pack.swatch} 0%, ${pack.swatchAlt} 100%)`,
                            }}
                            aria-hidden
                          />
                          <span className="settings-skin-card__name">{label}</span>
                        </RadioGroupItem>
                      );
                    })}
                  </RadioGroup>
                </div>
              </div>
                {onWallpaper ? (
                  <div
                    className="settings-card settings-card--appearance-col"
                    id="settings-anchor-wallpaper"
                  >
                    <div className="settings-row settings-row--stack">
                      <div className="settings-row__text">
                        <div className="settings-row__label">
                          {t("settings.wallpaper")}
                        </div>
                        <div className="settings-row__desc">
                          {t("settings.wallpaperDesc")}
                        </div>
                      </div>
                      <div className="settings-wallpaper">
                        {/* 浏览器文件选择能力必须由不可见原生 input 承载。 */}
                        <input
                          ref={wallpaperInputRef}
                          type="file"
                          accept={WALLPAPER_ACCEPT}
                          hidden
                          onChange={(e) => {
                            void onWallpaperFile(e.target.files?.[0]);
                          }}
                        />
                        <div className="settings-wallpaper__preview-wrap">
                          {wallpaperUrl ? (
                            <div
                              className={
                                "settings-wallpaper__preview settings-wallpaper__preview--set" +
                                (wallpaperBusy
                                  ? " settings-wallpaper__preview--busy"
                                  : "")
                              }
                            >
                              <WallpaperMediaLayer
                                url={wallpaperUrl}
                                kind={wallpaperKind ?? "image"}
                                focus={
                                  wallpaperFocus ?? DEFAULT_WALLPAPER_FOCUS
                                }
                                clip={wallpaperClip}
                                intrinsicSize={wallpaperMediaSize}
                                onIntrinsicSize={onWallpaperMediaSize}
                                className="settings-wallpaper__media"
                                mediaClassName="settings-wallpaper__media-el"
                              />
                              {wallpaperBusy ? (
                                <span
                                  className="settings-wallpaper__busy"
                                  aria-hidden
                                >
                                  {t("settings.wallpaperWorking")}
                                </span>
                              ) : null}
                              <div className="settings-wallpaper__hover">
                                <Button
                                  type="button"
                                  className="btn btn--solid btn--sm"
                                  disabled={wallpaperBusy}
                                  onClick={() =>
                                    wallpaperInputRef.current?.click()
                                  }
                                >
                                  {t("settings.wallpaperReplace")}
                                </Button>
                                {onWallpaperAdjust ? (
                                  <Button
                                    type="button"
                                    className="btn btn--solid btn--sm"
                                    disabled={wallpaperBusy}
                                    onClick={() => setWallpaperFocusOpen(true)}
                                  >
                                    <IconCrop size={14} />
                                    {t("settings.wallpaperFocus")}
                                  </Button>
                                ) : null}
                              </div>
                              <Button
                                type="button"
                                className="settings-wallpaper__clear btn btn--ghost btn--sm"
                                disabled={wallpaperBusy}
                                onClick={() => {
                                  setWallpaperError(null);
                                  setWallpaperFocusOpen(false);
                                  void onWallpaper(null);
                                }}
                              >
                                {t("settings.wallpaperClear")}
                              </Button>
                            </div>
                          ) : (
                            <Button
                              type="button"
                              className={
                                "settings-wallpaper__preview" +
                                (wallpaperBusy
                                  ? " settings-wallpaper__preview--busy"
                                  : "")
                              }
                              disabled={wallpaperBusy}
                              aria-label={
                                wallpaperBusy
                                  ? t("settings.wallpaperWorking")
                                  : t("settings.wallpaperUpload")
                              }
                              onClick={() =>
                                wallpaperInputRef.current?.click()
                              }
                            >
                              <span className="settings-wallpaper__preview-empty">
                                {wallpaperBusy
                                  ? t("settings.wallpaperWorking")
                                  : t("settings.wallpaperEmpty")}
                              </span>
                            </Button>
                          )}
                        </div>
                        {wallpaperUrl && onWallpaperAdjust ? (
                          <WallpaperFocusEditor
                            open={wallpaperFocusOpen}
                            onClose={() => setWallpaperFocusOpen(false)}
                            onApply={(result) => onWallpaperAdjust(result)}
                            mediaUrl={wallpaperUrl}
                            kind={wallpaperKind ?? "image"}
                            initialFocus={
                              wallpaperFocus ?? DEFAULT_WALLPAPER_FOCUS
                            }
                            initialClip={wallpaperClip}
                            labels={{
                              title: t("settings.wallpaperFocusTitle"),
                              hint: t("settings.wallpaperFocusHint"),
                              hintVideo: t("settings.wallpaperFocusHintVideo"),
                              zoom: t("settings.wallpaperFocusZoom"),
                              clip: t("settings.wallpaperClip"),
                              clipStart: t("settings.wallpaperClipStart"),
                              clipEnd: t("settings.wallpaperClipEnd"),
                              reset: t("settings.wallpaperFocusReset"),
                              cancel: t("common.cancel"),
                              apply: t("settings.wallpaperFocusApply"),
                              close: t("common.close"),
                            }}
                          />
                        ) : null}
                        {wallpaperUrl && onWallpaperScrim && onWallpaperBlur ? (
                          <div className="settings-wallpaper__scrim">
                            <div className="settings-wallpaper__scrim-head">
                              <Label
                                className="settings-wallpaper__scrim-label"
                                htmlFor="settings-wallpaper-scrim"
                              >
                                {t("settings.wallpaperVisibility")}
                              </Label>
                              <span
                                className="settings-wallpaper__scrim-value"
                                aria-hidden
                              >
                                {100 - Math.round(wallpaperScrim)}%
                              </span>
                            </div>
                            <Slider
                              id="settings-wallpaper-scrim"
                              className="settings-wallpaper__scrim-range"
                              min={0}
                              max={100}
                              step={1}
                              value={[100 - wallpaperScrim]}
                              aria-valuemin={0}
                              aria-valuemax={100}
                              aria-valuenow={100 - Math.round(wallpaperScrim)}
                              aria-label={t("settings.wallpaperVisibility")}
                              onValueChange={([value]) => {
                                onWallpaperScrim(100 - value);
                              }}
                            />
                            <div className="settings-wallpaper__scrim-head">
                              <Label
                                className="settings-wallpaper__scrim-label"
                                htmlFor="settings-wallpaper-blur"
                              >
                                {t("settings.wallpaperBlur")}
                              </Label>
                              <span
                                className="settings-wallpaper__scrim-value"
                                aria-hidden
                              >
                                {Math.round(wallpaperBlur)}px
                              </span>
                            </div>
                            <Slider
                              id="settings-wallpaper-blur"
                              className="settings-wallpaper__scrim-range"
                              min={0}
                              max={24}
                              step={1}
                              value={[wallpaperBlur]}
                              aria-label={t("settings.wallpaperBlur")}
                              onValueChange={([value]) =>
                                onWallpaperBlur(value)
                              }
                            />
                            <p className="settings-wallpaper__scrim-hint">
                              {t("settings.wallpaperScrimDesc")}
                            </p>
                            {onWallpaperAppearanceReset ? (
                              <Button
                                type="button"
                                className="btn btn--ghost btn--sm"
                                onClick={onWallpaperAppearanceReset}
                              >
                                {t("settings.wallpaperAppearanceReset")}
                              </Button>
                            ) : null}
                          </div>
                        ) : null}
                        {wallpaperError ? (
                          <p
                            className="settings-wallpaper__error"
                            role="alert"
                          >
                            {wallpaperError}
                          </p>
                        ) : null}
                      </div>
                    </div>
                  </div>
                ) : null}
            </div>
          </>
        )}

        {section === "account" && (
          <div id="settings-anchor-account-providers">
            <p className="settings-page__lead">
              {t("settings.tabProvidersHint")}
            </p>
            <ProvidersPanel
              locale={locale}
              onProviderActivated={onProviderActivated}
            />
          </div>
        )}

        {section === "personalization" && (
          <div className="settings-search-target">
            <PersonalizationSettingsPanel
              value={customInstructions}
              locale={locale}
              onSave={onCustomInstructionsSave}
              localMemories={localMemories}
              onLocalMemoriesChange={onLocalMemoriesChange}
              memoryFile={memoryFile}
              onMemoryFileSave={onMemoryFileSave}
              onMemoriesReset={onMemoriesReset}
            />
          </div>
        )}

        {section === "analytics" && (
          <div
            id="settings-anchor-analytics"
            className="settings-search-target"
          >
            <AnalyticsSettingsPanel
              locale={locale}
              labels={{
                loading: t("settings.analytics.loading"),
                empty: t("settings.analytics.empty"),
                totalRequests: t("settings.analytics.totalRequests"),
                totalTokens: t("settings.analytics.totalTokens"),
                byModel: t("settings.analytics.byModel"),
                byDay: t("settings.analytics.byDay"),
                activityHeatmap: t("settings.analytics.activityHeatmap"),
                less: t("settings.analytics.less"),
                more: t("settings.analytics.more"),
                tokenTrend: t("settings.analytics.tokenTrend"),
                modelUsage: t("settings.analytics.modelUsage"),
                rounds: t("settings.analytics.rounds"),
              }}
            />
          </div>
        )}

        {section === "requests" && (
          <RequestHistoryPanel
            locale={locale}
            labels={{
              loading: t("settings.requests.loading"),
              error: t("settings.requests.error"),
              empty: t("settings.requests.empty"),
              refresh: t("settings.requests.refresh"),
              refreshing: t("settings.requests.refreshing"),
              invalidDateRange: t("settings.requests.invalidDateRange"),
              filters: t("settings.requests.filters"),
              model: t("settings.requests.model"),
              status: t("settings.requests.status"),
              from: t("settings.requests.from"),
              to: t("settings.requests.to"),
              allModels: t("settings.requests.allModels"),
              allStatuses: t("settings.requests.allStatuses"),
              clearFilters: t("settings.requests.clearFilters"),
              time: t("settings.requests.time"),
              provider: t("settings.requests.provider"),
              requestMode: t("settings.requests.requestMode"),
              stream: t("settings.requests.stream"),
              sync: t("settings.requests.sync"),
              attempt: t("settings.requests.attempt"),
              duration: t("settings.requests.duration"),
              tokens: t("settings.requests.tokens"),
              details: t("settings.requests.details"),
              close: t("common.close"),
              purpose: t("settings.requests.purpose"),
              protocol: t("settings.requests.protocol"),
              endpoint: t("settings.requests.endpoint"),
              logicalRequestId: t("settings.requests.logicalRequestId"),
              sessionId: t("settings.requests.sessionId"),
              turnId: t("settings.requests.turnId"),
              agentId: t("settings.requests.agentId"),
              firstResponse: t("settings.requests.firstResponse"),
              firstResponseDuration: t("settings.requests.firstResponseDuration"),
              completedAt: t("settings.requests.completedAt"),
              httpStatus: t("settings.requests.httpStatus"),
              providerRequestId: t("settings.requests.providerRequestId"),
              errorKind: t("settings.requests.errorKind"),
              errorDetail: t("settings.requests.errorDetail"),
              cacheCreation: t("settings.requests.cacheCreation"),
              cacheRead: t("settings.requests.cacheRead"),
              inputTokens: t("settings.requests.inputTokens"),
              outputTokens: t("settings.requests.outputTokens"),
              reasoningTokens: t("settings.requests.reasoningTokens"),
              notReported: t("settings.requests.notReported"),
              previous: t("settings.requests.previous"),
              next: t("settings.requests.next"),
              range: t("settings.requests.range"),
              statusSuccess: t("settings.requests.statusSuccess"),
              statusRunning: t("settings.requests.statusRunning"),
              statusFailed: t("settings.requests.statusFailed"),
              statusCancelled: t("settings.requests.statusCancelled"),
              statusConnection: t("settings.requests.statusConnection"),
              statusTimeout: t("settings.requests.statusTimeout"),
              statusTls: t("settings.requests.statusTls"),
              statusTransport: t("settings.requests.statusTransport"),
              statusHttp: t("settings.requests.statusHttp"),
              statusProtocol: t("settings.requests.statusProtocol"),
              statusStreamInterrupted: t("settings.requests.statusStreamInterrupted"),
              statusRetryExhausted: t("settings.requests.statusRetryExhausted"),
              statusOther: t("settings.requests.statusOther"),
            }}
          />
        )}

        {(section === "market" ||
          section === "plugins" ||
          section === "skills" ||
          section === "mcp") && (
          <ExtensionsPanel
            locale={locale}
            projectPath={projectPath}
            activeTab={section}
          />
        )}

        {section === "agents" && (
          <AgentsPanel locale={locale} />
        )}

        {section === "about" && (
          <div
            className="settings-card"
            id="settings-anchor-about"
          >
            <div className="settings-row settings-row--stack">
              <div className="settings-row__text">
                <div className="settings-row__label">
                  <IconInfo size={16} />
                  {t("settings.aboutApp")}
                </div>
                <div className="settings-row__desc settings-about__tagline">
                  {t("settings.aboutTagline")}
                </div>
                <div className="settings-row__desc">
                  {t("settings.aboutDescription")}
                </div>
                <div className="settings-about__features">
                  <div className="settings-about__feature">
                    <div className="settings-about__feature-title">
                      {t("settings.aboutLocalTitle")}
                    </div>
                    <div className="settings-about__feature-desc">
                      {t("settings.aboutLocalDesc")}
                    </div>
                  </div>
                  <div className="settings-about__feature">
                    <div className="settings-about__feature-title">
                      {t("settings.aboutOpenTitle")}
                    </div>
                    <div className="settings-about__feature-desc">
                      {t("settings.aboutOpenDesc")}
                    </div>
                  </div>
                  <div className="settings-about__feature">
                    <div className="settings-about__feature-title">
                      {t("settings.aboutLightTitle")}
                    </div>
                    <div className="settings-about__feature-desc">
                      {t("settings.aboutLightDesc")}
                    </div>
                  </div>
                  <div className="settings-about__feature">
                    <div className="settings-about__feature-title">
                      {t("settings.aboutSourceTitle")}
                    </div>
                    <div className="settings-about__feature-desc">
                      {t("settings.aboutSourceDesc")}
                    </div>
                    <Button
                      type="button"
                      className="settings-about__source-link"
                      onClick={() => {
                        if (isTauri()) {
                          void urlOpen(SOURCE_REPOSITORY_URL);
                        } else {
                          window.open(
                            SOURCE_REPOSITORY_URL,
                            "_blank",
                            "noopener,noreferrer",
                          );
                        }
                      }}
                    >
                      <IconExternalLink size={14} />
                      {SOURCE_REPOSITORY_URL}
                    </Button>
                  </div>
                </div>
                <div className="settings-row__hint settings-about__version">
                  {versionFooter}
                </div>
                <AppUpdateSection
                  locale={locale}
                  status={appUpdateStatus}
                  busy={appUpdateBusy}
                  error={appUpdateError}
                  downloadSourcePreference={appUpdateDownloadSource}
                  onDownloadSourcePreferenceChange={onAppUpdateDownloadSource}
                  onCheck={onAppUpdateCheck}
                  onInstall={onAppUpdateInstall}
                />
              </div>
            </div>
          </div>
        )}
      </main>
      </div>

    </div>
  );
}

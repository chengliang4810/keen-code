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
  IconInfo,
  IconPlug,
  IconPuzzle,
  IconSettings,
  IconSkills,
  IconSummary,
  IconUser,
} from "@/components/icons";
import type { ThemePreference } from "@/lib/theme";
import {
  DEFAULT_WALLPAPER_FOCUS,
  THEME_SKINS,
  WALLPAPER_ACCEPT,
  WallpaperPrepareError,
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
import { PersonalizationSettingsPanel } from "@/components/PersonalizationSettingsPanel";
import {
  AppUpdateSection,
  type AppUpdateBusy,
} from "@/components/AppUpdateSection";
import type { AppUpdateDownloadSource, AppUpdateStatus } from "@/lib/api";
import {
  createT,
  type Locale,
  type MessageKey,
  type Vars,
} from "@/i18n";
import {
  SETTINGS_NAV,
  buildSettingsHash,
  getNavDef,
  type SettingsNavIcon,
  type SettingsSectionId,
} from "@/lib/settingsCatalog";

export type { SettingsSectionId } from "@/lib/settingsCatalog";

export interface SettingsPageProps {
  section: SettingsSectionId;
  /** 跳转到一个当前设置分区。 */
  onSection: (id: SettingsSectionId) => void;
  onBack: () => void;
  locale: Locale;
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
  /** Windows WebView2 是否启用硬件加速。 */
  chromeHardwareAcceleration?: boolean;
  onChromeHardwareAcceleration?: (v: boolean) => void;
  /** 是否展示每轮全部思考片段。 */
  showFullThinking?: boolean;
  onShowFullThinking?: (v: boolean) => void;
  /** 是否发送任务完成、失败和等待确认的桌面通知。 */
  taskNotifications?: boolean;
  onTaskNotifications?: (v: boolean) => void;
  /** 任务通知是否播放系统默认提示音。 */
  notificationSound?: boolean;
  onNotificationSound?: (v: boolean) => void;
  /** 是否阻止系统因用户空闲自动进入睡眠。 */
  keepComputerAwake?: boolean;
  onKeepComputerAwake?: (v: boolean) => void;
  /** 是否自动归档符合条件的旧任务。 */
  autoArchiveOldTasks?: boolean;
  onAutoArchiveOldTasks?: (v: boolean) => void;
  /** 自动归档前的未更新保留天数。 */
  archiveRetentionDays?: number;
  onArchiveRetentionDays?: (v: number) => void;
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
  if (name === "agents") return <IconUser size={size} />;
  if (name === "mcp") return <IconPlug size={size} />;
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
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      title={ariaLabel}
      disabled={disabled}
      className={"ext-switch" + (checked ? " is-on" : "")}
      onClick={(event) => {
        event.stopPropagation();
        onChange(!checked);
      }}
      onPointerDown={(event) => event.stopPropagation()}
    >
      <span className="ext-switch__thumb" aria-hidden />
    </button>
  );
}

export function SettingsPage({
  section,
  onSection,
  onBack,
  locale,
  themePreference,
  onTheme,
  skin,
  onSkin,
  wallpaperUrl = null,
  wallpaperKind = null,
  wallpaperFocus = null,
  wallpaperClip = null,
  wallpaperMediaSize = null,
  wallpaperScrim = 100,
  onWallpaperScrim,
  onWallpaper,
  onWallpaperAdjust,
  onWallpaperMediaSize,
  chromeHardwareAcceleration = true,
  onChromeHardwareAcceleration,
  showFullThinking = true,
  onShowFullThinking,
  taskNotifications = true,
  onTaskNotifications,
  notificationSound = true,
  onNotificationSound,
  keepComputerAwake = false,
  onKeepComputerAwake,
  autoArchiveOldTasks = true,
  onAutoArchiveOldTasks,
  archiveRetentionDays = 7,
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
}: SettingsPageProps) {
  /** Pending scroll target after search jump / deep link. */
  const pendingAnchorRef = useRef<string | null>(null);
  const [highlightAnchor, setHighlightAnchor] = useState<string | null>(null);
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
    (id: SettingsSectionId, anchorId?: string | null) => {
      if (anchorId) pendingAnchorRef.current = anchorId;
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

  // 分区切换后滚动到请求的设置项并短暂高亮。
  useEffect(() => {
    const anchor = pendingAnchorRef.current;
    if (!anchor) return;
    pendingAnchorRef.current = null;
    const timer = window.setTimeout(() => {
      const el = document.getElementById(anchor);
      if (!el) return;
      el.scrollIntoView({ block: "center", behavior: "smooth" });
      setHighlightAnchor(anchor);
      window.setTimeout(() => setHighlightAnchor(null), 1600);
    }, 60);
    return () => window.clearTimeout(timer);
  }, [section]);

  const nav = SETTINGS_NAV;

  const personalNav = useMemo(
    () => nav.filter((n) => n.group === "personal"),
    [nav],
  );
  const systemNav = useMemo(
    () => nav.filter((n) => n.group === "system"),
    [nav],
  );

  const rowHighlight = useCallback(
    (anchorId: string) =>
      highlightAnchor === anchorId ? " is-search-hit" : "",
    [highlightAnchor],
  );
  const sectionNav = getNavDef(section);
  if (!sectionNav) {
    throw new Error(`未注册的设置分区：${section}`);
  }
  const title = t(sectionNav.labelKey);
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
    <button
      key={n.id}
      type="button"
      className={
        "settings-page__nav-item" +
        (section === n.id ? " is-active" : "")
      }
      onClick={() => openSection(n.id)}
    >
      <NavIcon name={n.icon} />
      <span className="settings-page__nav-label">{t(n.labelKey)}</span>
    </button>
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
      <aside className="settings-page__nav">
        <div className="settings-page__nav-inner">
        <button
          type="button"
          className="settings-page__back"
          onClick={onBack}
        >
          <IconArrowLeft size={16} />
          <span>{t("settings.backToApp")}</span>
        </button>

        {personalNav.length > 0 ? (
          <>
            <div className="settings-page__group-label">
              {t("settings.group.personal")}
            </div>
            {personalNav.map(renderNavItem)}
          </>
        ) : null}

        {systemNav.length > 0 ? (
          <>
            <div className="settings-page__group-label">
              {t("settings.group.system")}
            </div>
            {systemNav.map(renderNavItem)}
          </>
        ) : null}

        </div>
      </aside>

      <div className="settings-page__content">
      <main className="settings-page__main">
        <h1 className="settings-page__title">{title}</h1>


        {section === "general" && (
          <>
            <h2 className="settings-page__h2">
              {t("settings.general.system")}
            </h2>
            <div className="settings-card">
              <div
                className={
                  "settings-row" +
                  rowHighlight("settings-anchor-hardware-acceleration")
                }
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
                  onChange={(checked) =>
                    onChromeHardwareAcceleration?.(checked)
                  }
                  ariaLabel={t("settings.chromeHardwareAcceleration")}
                />
              </div>
              <div className="settings-row" id="settings-anchor-task-notifications">
                <div className="settings-row__text">
                  <div className="settings-row__label">{t("settings.taskNotifications")}</div>
                  <div className="settings-row__desc">{t("settings.taskNotificationsDesc")}</div>
                </div>
                <SettingsSwitch
                  checked={taskNotifications}
                  onChange={(checked) => onTaskNotifications?.(checked)}
                  ariaLabel={t("settings.taskNotifications")}
                />
              </div>
              <div className="settings-row" id="settings-anchor-notification-sound">
                <div className="settings-row__text">
                  <div className="settings-row__label">{t("settings.notificationSound")}</div>
                  <div className="settings-row__desc">{t("settings.notificationSoundDesc")}</div>
                </div>
                <SettingsSwitch
                  checked={notificationSound}
                  disabled={!taskNotifications}
                  onChange={(checked) => onNotificationSound?.(checked)}
                  ariaLabel={t("settings.notificationSound")}
                />
              </div>
              <div className="settings-row" id="settings-anchor-keep-awake">
                <div className="settings-row__text">
                  <div className="settings-row__label">{t("settings.keepComputerAwake")}</div>
                  <div className="settings-row__desc">{t("settings.keepComputerAwakeDesc")}</div>
                </div>
                <SettingsSwitch
                  checked={keepComputerAwake}
                  onChange={(checked) => onKeepComputerAwake?.(checked)}
                  ariaLabel={t("settings.keepComputerAwake")}
                />
              </div>
              <div className="settings-row" id="settings-anchor-auto-archive">
                <div className="settings-row__text">
                  <div className="settings-row__label">{t("settings.autoArchiveOldTasks")}</div>
                  <div className="settings-row__desc">{t("settings.autoArchiveOldTasksDesc")}</div>
                </div>
                <SettingsSwitch
                  checked={autoArchiveOldTasks}
                  onChange={(checked) => onAutoArchiveOldTasks?.(checked)}
                  ariaLabel={t("settings.autoArchiveOldTasks")}
                />
              </div>
              <div
                className={"settings-row" + (!autoArchiveOldTasks ? " is-disabled" : "")}
                id="settings-anchor-archive-retention"
              >
                <div className="settings-row__text">
                  <div className="settings-row__label">{t("settings.archiveRetention")}</div>
                  <div className="settings-row__desc">{t("settings.archiveRetentionDesc")}</div>
                </div>
                <input
                  type="number"
                  className="settings-input settings-input--compact"
                  value={archiveRetentionDays}
                  min={1}
                  max={365}
                  step={1}
                  inputMode="numeric"
                  disabled={!autoArchiveOldTasks}
                  aria-label={t("settings.archiveRetention")}
                  onChange={(event) => {
                    const days = Number(event.target.value);
                    if (Number.isInteger(days) && days >= 1 && days <= 365) {
                      onArchiveRetentionDays?.(days);
                    }
                  }}
                />
              </div>
            </div>

            <h2 className="settings-page__h2">
              {t("settings.general.display")}
            </h2>
            <div className="settings-card">
              <div
                className={
                  "settings-row" +
                  rowHighlight("settings-anchor-show-full-thinking")
                }
                id="settings-anchor-show-full-thinking"
              >
                <div className="settings-row__text">
                  <div className="settings-row__label">
                    {t("settings.showFullThinking")}
                  </div>
                  <div className="settings-row__desc">
                    {t("settings.showFullThinkingDesc")}
                  </div>
                </div>
                <SettingsSwitch
                  checked={showFullThinking}
                  onChange={(checked) => onShowFullThinking?.(checked)}
                  ariaLabel={t("settings.showFullThinking")}
                />
              </div>
            </div>

          </>
        )}

        {section === "archived" && (
          <>
            <p className="settings-page__lead">{t("settings.archived.desc")}</p>
            <input
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
                    <button
                      type="button"
                      className="btn btn--solid btn--sm"
                      disabled={restoringSessionId !== null}
                      onClick={() => void restoreArchivedSession(archivedSession.id)}
                    >
                      {restoringSessionId === archivedSession.id
                        ? t("settings.archived.restoring")
                        : t("settings.archived.restore")}
                    </button>
                    <button
                      type="button"
                      className="btn btn--danger btn--sm"
                      disabled={restoringSessionId !== null}
                      onClick={() => onDeleteArchivedSession?.(archivedSession.id)}
                    >
                      {t("settings.archived.delete")}
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </>
        )}

        {section === "appearance" && (
          <>
            <div
              className={"settings-card" + rowHighlight("settings-anchor-theme")}
              id="settings-anchor-theme"
            >
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
                <div className="settings-seg" role="radiogroup" aria-label={t("settings.theme")}>
                  <button
                    type="button"
                    role="radio"
                    aria-checked={themePreference === "system"}
                    className={
                      "settings-seg__btn" +
                      (themePreference === "system" ? " is-on" : "")
                    }
                    onClick={() => onTheme("system")}
                  >
                    {t("settings.themeSystem")}
                  </button>
                  <button
                    type="button"
                    role="radio"
                    aria-checked={themePreference === "light"}
                    className={
                      "settings-seg__btn" +
                      (themePreference === "light" ? " is-on" : "")
                    }
                    onClick={() => onTheme("light")}
                  >
                    {t("settings.themeLight")}
                  </button>
                  <button
                    type="button"
                    role="radio"
                    aria-checked={themePreference === "dark"}
                    className={
                      "settings-seg__btn" +
                      (themePreference === "dark" ? " is-on" : "")
                    }
                    onClick={() => onTheme("dark")}
                  >
                    {t("settings.themeDark")}
                  </button>
                </div>
              </div>
            </div>
            <div className="settings-appearance-duo">
              <div
                className={
                  "settings-card settings-card--appearance-col" +
                  rowHighlight("settings-anchor-skin")
                }
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
                  <div
                    className="settings-skin-grid"
                    role="listbox"
                    aria-label={t("settings.skin")}
                  >
                    {THEME_SKINS.map((pack) => {
                      const selected = skin === pack.id;
                      const label = t(
                        `settings.skin.${pack.id}` as "settings.skin.default",
                      );
                      return (
                        <button
                          key={pack.id}
                          type="button"
                          role="option"
                          aria-selected={selected}
                          className={
                            "settings-skin-card" +
                            (selected ? " is-on" : "")
                          }
                          onClick={() => onSkin(pack.id)}
                        >
                          <span
                            className="settings-skin-card__swatch"
                            style={{
                              background: `linear-gradient(135deg, ${pack.swatch} 0%, ${pack.swatchAlt} 100%)`,
                            }}
                            aria-hidden
                          />
                          <span className="settings-skin-card__name">{label}</span>
                        </button>
                      );
                    })}
                  </div>
                </div>
              </div>
                {onWallpaper ? (
                  <div
                    className={
                      "settings-card settings-card--appearance-col" +
                      rowHighlight("settings-anchor-wallpaper")
                    }
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
                                <button
                                  type="button"
                                  className="btn btn--solid btn--sm"
                                  disabled={wallpaperBusy}
                                  onClick={() =>
                                    wallpaperInputRef.current?.click()
                                  }
                                >
                                  {t("settings.wallpaperReplace")}
                                </button>
                                {onWallpaperAdjust ? (
                                  <button
                                    type="button"
                                    className="btn btn--solid btn--sm"
                                    disabled={wallpaperBusy}
                                    onClick={() => setWallpaperFocusOpen(true)}
                                  >
                                    <IconCrop size={14} />
                                    {t("settings.wallpaperFocus")}
                                  </button>
                                ) : null}
                              </div>
                              <button
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
                              </button>
                            </div>
                          ) : (
                            <button
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
                            </button>
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
                        {wallpaperUrl && onWallpaperScrim ? (
                          <div className="settings-wallpaper__scrim">
                            <div className="settings-wallpaper__scrim-head">
                              <label
                                className="settings-wallpaper__scrim-label"
                                htmlFor="settings-wallpaper-scrim"
                              >
                                {t("settings.wallpaperScrim")}
                              </label>
                              <span
                                className="settings-wallpaper__scrim-value"
                                aria-hidden
                              >
                                {Math.round(wallpaperScrim)}%
                              </span>
                            </div>
                            <input
                              id="settings-wallpaper-scrim"
                              type="range"
                              className="settings-wallpaper__scrim-range"
                              min={0}
                              max={100}
                              step={1}
                              value={wallpaperScrim}
                              aria-valuemin={0}
                              aria-valuemax={100}
                              aria-valuenow={Math.round(wallpaperScrim)}
                              aria-label={t("settings.wallpaperScrim")}
                              onChange={(e) => {
                                onWallpaperScrim(Number(e.target.value));
                              }}
                            />
                            <p className="settings-wallpaper__scrim-hint">
                              {t("settings.wallpaperScrimDesc")}
                            </p>
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
          <PersonalizationSettingsPanel
            value={customInstructions}
            locale={locale}
            onSave={onCustomInstructionsSave}
          />
        )}

        {section === "analytics" && (
          <AnalyticsSettingsPanel
            mode="usage"
            labels={{
              loading: t("settings.analytics.loading"),
              empty: t("settings.analytics.empty"),
              time: t("settings.analytics.time"),
              model: t("settings.analytics.model"),
              requestMode: t("settings.analytics.requestMode"),
              duration: t("settings.analytics.duration"),
              tokens: t("settings.analytics.tokens"),
              details: t("settings.analytics.details"),
              sync: t("settings.analytics.sync"),
              async: t("settings.analytics.async"),
              estimated: t("settings.analytics.estimated"),
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
        )}

        {(section === "market" ||
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
            className={"settings-card" + rowHighlight("settings-anchor-about")}
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
                      {t("settings.aboutWorkflowTitle")}
                    </div>
                    <div className="settings-about__feature-desc">
                      {t("settings.aboutWorkflowDesc")}
                    </div>
                  </div>
                  <div className="settings-about__feature">
                    <div className="settings-about__feature-title">
                      {t("settings.aboutSourceTitle")}
                    </div>
                    <div className="settings-about__feature-desc">
                      {t("settings.aboutSourceDesc")}
                    </div>
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

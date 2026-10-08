// Modified for RCode. See NOTICE.
import { useTranslation, type LanguagePreference } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import {
  type OsNotificationResult,
  testAgentOsNotification,
} from "@/modules/agents/lib/notify";
import { usePreferencesStore } from "@/modules/settings/preferences";
import type { ThemePref } from "@/modules/settings/store";
import {
  setAgentNotificationSound,
  setAgentNotifications,
  setAutostart,
  setConfirmCloseRunningTerminal,
  setDefaultWorkspaceEnv,
  setExplorerGitDecorations,
  setRestoreWindowState,
  setShowHidden,
  setTerminalCursorBlink,
  setTerminalCursorStyle,
  setTerminalFontFamily,
  setTerminalFontSize,
  setTerminalFontWeight,
  setTerminalLetterSpacing,
  setTerminalScrollback,
  setTerminalShell,
  setTerminalRenderer,
  setTerminalScreenReader,
  setZoomLevel,
  setUiLanguage,
  setUiFontSize,
  TERMINAL_FONT_SIZES,
  TERMINAL_SCROLLBACK_PRESETS,
} from "@/modules/settings/store";
import {
  UI_FONT_SIZE_DEFAULT,
  UI_FONT_SIZES,
} from "@/modules/settings/uiFontSize";
import { useTheme } from "@/modules/theme";
import {
  ComputerIcon,
  Moon02Icon,
  Sun03Icon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { invoke } from "@tauri-apps/api/core";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { useEffect, useState } from "react";
import { SectionHeader } from "../components/SectionHeader";
import { SettingRow } from "../components/SettingRow";
import { SettingGroup } from "@/settings/components/SettingGroup";

const APPEARANCE: {
  id: ThemePref;
  label: string;
  icon: typeof ComputerIcon;
}[] = [
  { id: "system", label: "System", icon: ComputerIcon },
  { id: "light", label: "Light", icon: Sun03Icon },
  { id: "dark", label: "Dark", icon: Moon02Icon },
];

const TERMINAL_FONT_WEIGHTS = [
  { value: "normal", label: "Normal" },
  { value: "500", label: "Medium" },
  { value: "600", label: "Semi-Bold" },
  { value: "bold", label: "Bold" },
] as const;
const TERMINAL_CURSOR_STYLES = [
  { value: "bar", label: "Bar" },
  { value: "block", label: "Block" },
  { value: "underline", label: "Underline" },
] as const;
const LETTER_SPACINGS = [-4, -3, -2, -1, 0, 1, 2, 3, 4] as const;

type ShellInfo = { name: string; path: string; integrated: boolean };
const SHELL_AUTO = "auto";
const ZOOM_MIN = 0.5;
const ZOOM_MAX = 2.0;
const ZOOM_STEP = 0.05;
const NOTIFICATION_TEST_DELAY_MS = 2_000;

type NotificationTestState =
  | OsNotificationResult
  | "idle"
  | "waiting"
  | "sending";

export function GeneralSection() {
  const tr = useTranslation();
  const { mode, setMode } = useTheme();
  const uiLanguage = usePreferencesStore((s) => s.uiLanguage);
  const hydrated = usePreferencesStore((s) => s.hydrated);
  const uiFontSize = usePreferencesStore((s) => s.uiFontSize);
  const [fontSizeSaving, setFontSizeSaving] = useState(false);
  const [fontSizeError, setFontSizeError] = useState<string | null>(null);
  const changeFontSize = async (value: string) => {
    setFontSizeSaving(true);
    setFontSizeError(null);
    try {
      await setUiFontSize(Number(value));
    } catch (error) {
      setFontSizeError(String(error));
    } finally {
      setFontSizeSaving(false);
    }
  };
  const [languageSaving, setLanguageSaving] = useState(false);
  const [languageError, setLanguageError] = useState<string | null>(null);
  const changeLanguage = async (value: LanguagePreference) => {
    setLanguageSaving(true);
    setLanguageError(null);
    try {
      await setUiLanguage(value);
    } catch (error) {
      setLanguageError(String(error));
    } finally {
      setLanguageSaving(false);
    }
  };

  const autostart = usePreferencesStore((s) => s.autostart);
  const restoreWindowState = usePreferencesStore((s) => s.restoreWindowState);
  const showHidden = usePreferencesStore((s) => s.showHidden);
  const explorerGitDecorations = usePreferencesStore(
    (s) => s.explorerGitDecorations,
  );
  const terminalRenderer = usePreferencesStore((s) => s.terminalRenderer);
  const terminalScreenReader = usePreferencesStore(
    (s) => s.terminalScreenReader,
  );
  const terminalCursorBlink = usePreferencesStore((s) => s.terminalCursorBlink);
  const terminalCursorStyle = usePreferencesStore((s) => s.terminalCursorStyle);
  const terminalFontFamily = usePreferencesStore((s) => s.terminalFontFamily);
  const terminalFontWeight = usePreferencesStore((s) => s.terminalFontWeight);
  const terminalShell = usePreferencesStore((s) => s.terminalShell);
  const [shells, setShells] = useState<ShellInfo[]>([]);
  const [wslDistros, setWslDistros] = useState<{ name: string }[]>([]);
  const defaultWorkspaceEnv = usePreferencesStore((s) => s.defaultWorkspaceEnv);
  const terminalLetterSpacing = usePreferencesStore(
    (s) => s.terminalLetterSpacing,
  );
  const terminalFontSize = usePreferencesStore((s) => s.terminalFontSize);
  const terminalScrollback = usePreferencesStore((s) => s.terminalScrollback);
  const confirmCloseRunningTerminal = usePreferencesStore(
    (s) => s.confirmCloseRunningTerminal,
  );
  const zoomLevel = usePreferencesStore((s) => s.zoomLevel);
  const agentNotifications = usePreferencesStore((s) => s.agentNotifications);
  const agentNotificationSound = usePreferencesStore(
    (s) => s.agentNotificationSound,
  );
  const [notificationTest, setNotificationTest] =
    useState<NotificationTestState>("idle");
  const notificationTestPending =
    notificationTest === "waiting" || notificationTest === "sending";

  const testNotification = async () => {
    setNotificationTest("waiting");
    await new Promise((resolve) =>
      setTimeout(resolve, NOTIFICATION_TEST_DELAY_MS),
    );
    setNotificationTest("sending");
    setNotificationTest(await testAgentOsNotification(agentNotificationSound));
  };

  useEffect(() => {
    let alive = true;
    void isEnabled()
      .then((on) => {
        if (!alive) return;
        if (on !== usePreferencesStore.getState().autostart) {
          void setAutostart(on);
        }
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    void invoke<ShellInfo[]>("pty_list_shells")
      .then(setShells)
      .catch(() => {});
    void invoke<{ name: string }[]>("wsl_list_distros")
      .then(setWslDistros)
      .catch(() => {});
  }, []);

  const onToggleAutostart = async (next: boolean) => {
    try {
      if (next) await enable();
      else await disable();
      await setAutostart(next);
    } catch (e) {
      console.error("autostart toggle failed", e);
    }
  };

  return (
    <div className="flex flex-col gap-6">
      <SectionHeader
        title={tr("General")}
        description={tr("Mode, terminal, and startup.")}
      />

      <SettingRow
        title={tr("Language")}
        description={tr(
          "Choose the interface language. Changes apply to all windows immediately.",
        )}
      >
        <Select
          disabled={!hydrated || languageSaving}
          value={uiLanguage}
          onValueChange={(value) =>
            void changeLanguage(value as LanguagePreference)
          }
        >
          <SelectTrigger className="w-40" aria-label={tr("Language")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="system">{tr("Follow system")}</SelectItem>
            <SelectItem value="zh-CN">简体中文</SelectItem>
            <SelectItem value="en-US">English</SelectItem>
          </SelectContent>
        </Select>
        {languageError && (
          <p role="alert" className="max-w-64 text-ui-sm text-destructive">
            {tr("Unable to change interface language: {detail}", {
              detail: languageError,
            })}
          </p>
        )}
      </SettingRow>

      <div className="flex flex-col gap-2">
        <Label>{tr("Appearance")}</Label>
        <div className="grid grid-cols-3 gap-2">
          {APPEARANCE.map((o) => (
            <button
              key={o.id}
              type="button"
              onClick={() => setMode(o.id)}
              className={cn(
                "group flex h-20 flex-col items-center justify-center gap-1.5 rounded-lg border bg-card transition-all",
                mode === o.id
                  ? "border-foreground/60 ring-1 ring-foreground/20"
                  : "border-border/60 hover:border-border",
              )}
            >
              <HugeiconsIcon icon={o.icon} size={18} strokeWidth={1.5} />
              <span className="text-ui-base">{tr(o.label)}</span>
            </button>
          ))}
        </div>
        <p className="text-ui-sm text-muted-foreground">
          {tr("For theme, background and customization, see the")}{" "}
          <strong className="font-medium text-foreground">
            {tr("Themes")}
          </strong>{" "}
          {tr("tab.")}
        </p>
      </div>

      <SettingRow
        title={tr("Interface font size")}
        description={tr(
          "Adjust text throughout the interface. Terminal and editor text use their own font sizes.",
        )}
      >
        <div className="flex flex-col items-end gap-1">
          <Select
            disabled={!hydrated || fontSizeSaving}
            value={String(uiFontSize)}
            onValueChange={(value) => void changeFontSize(value)}
          >
            <SelectTrigger
              className="min-w-32"
              aria-label={tr("Interface font size")}
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {UI_FONT_SIZES.map((size) => (
                <SelectItem key={size} value={String(size)}>
                  {size} {tr("px")}
                  {size === UI_FONT_SIZE_DEFAULT ? ` (${tr("Default")})` : ""}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {fontSizeError && (
            <p role="alert" className="max-w-64 text-ui-sm text-destructive">
              {tr("Unable to change interface font size: {detail}", {
                detail: fontSizeError,
              })}
            </p>
          )}
        </div>
      </SettingRow>

      <div className="flex flex-col gap-2">
        <Label>{tr("Zoom")}</Label>
        <div className="flex flex-col gap-3 rounded-lg border border-border/60 p-3">
          <div className="flex items-center justify-between gap-3">
            <span className="text-ui-base text-muted-foreground">
              {tr("UI zoom level")}
            </span>
            <span className="tabular-nums text-ui-sm text-muted-foreground">
              {Math.round(zoomLevel * 100)}%
            </span>
          </div>
          <Slider
            aria-label={tr("UI zoom level")}
            value={[zoomLevel]}
            min={ZOOM_MIN}
            max={ZOOM_MAX}
            step={ZOOM_STEP}
            onValueChange={(v) => void setZoomLevel(v[0] ?? 1)}
          />
        </div>
      </div>

      <SettingGroup title={tr("Explorer")}>
        <SettingRow
          title={tr("Show hidden files")}
          description={tr(
            "Include dot-prefixed files and folders (.env, .gitignore, .config) in the file explorer and search.",
          )}
        >
          <Switch
            checked={showHidden}
            onCheckedChange={(v) => void setShowHidden(v)}
          />
        </SettingRow>
        <SettingRow
          title={tr("Git decorations")}
          description={tr(
            "Tint changed files and dim gitignored entries in the file explorer.",
          )}
        >
          <Switch
            checked={explorerGitDecorations}
            onCheckedChange={(v) => void setExplorerGitDecorations(v)}
          />
        </SettingRow>
      </SettingGroup>

      <SettingGroup title={tr("Terminal")}>
        <SettingRow
          title={tr("Terminal renderer")}
          description={tr(
            "Automatic uses WebGPU with WebGL fallback. Choose WebGL for graphics compatibility. Applies to new terminals.",
          )}
        >
          <Select
            value={terminalRenderer}
            onValueChange={(value) =>
              void setTerminalRenderer(value === "webgl" ? "webgl" : "auto")
            }
          >
            <SelectTrigger className="h-8 w-36 text-ui-base">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="auto">{tr("Automatic")}</SelectItem>
              <SelectItem value="webgl">{tr("WebGL")}</SelectItem>
            </SelectContent>
          </Select>
        </SettingRow>
        <SettingRow
          title={tr("Screen reader support")}
          description={tr(
            "Expose terminal output as accessible text. Page Up and Page Down browse history when the output region is focused.",
          )}
        >
          <Switch
            checked={terminalScreenReader}
            onCheckedChange={(value) => void setTerminalScreenReader(value)}
          />
        </SettingRow>
        <SettingRow title={tr("Cursor blinking")} description="">
          <Switch
            checked={terminalCursorBlink}
            onCheckedChange={(v) => void setTerminalCursorBlink(v)}
          />
        </SettingRow>
        <SettingRow
          title={tr("Cursor style")}
          description={tr("Shape of the terminal cursor.")}
        >
          <Select
            value={terminalCursorStyle}
            onValueChange={(v) => void setTerminalCursorStyle(v)}
          >
            <SelectTrigger
              value={terminalCursorStyle}
              className="h-8 w-28 text-ui-base"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {TERMINAL_CURSOR_STYLES.map((style) => (
                <SelectItem
                  key={style.value}
                  value={style.value}
                  className="text-ui-base"
                >
                  {tr(style.label)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingRow>
        <FontFamilyInput
          value={terminalFontFamily}
          onCommit={(v) => void setTerminalFontFamily(v)}
        />
        <SettingRow
          title={tr("Font weight")}
          description={tr("Thickness of terminal characters")}
        >
          <Select
            value={terminalFontWeight}
            onValueChange={(v) => void setTerminalFontWeight(v)}
          >
            <SelectTrigger
              value={terminalFontWeight}
              className="h-8 w-28 text-ui-base"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {TERMINAL_FONT_WEIGHTS.map((w) => (
                <SelectItem
                  key={w.value}
                  value={w.value}
                  className="text-ui-base"
                >
                  {tr(w.label)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingRow>
        <SettingRow
          title={tr("Integrated terminal shell")}
          description={
            shells.find((s) => s.path === terminalShell)?.integrated === false
              ? tr(
                  "Command blocks and directory tracking are unavailable for this shell.",
                )
              : wslDistros.length > 0
                ? tr(
                    "Shell for the integrated terminal. WSL spaces use the distro login shell. Existing tabs keep their shell.",
                  )
                : tr(
                    "Shell for new terminal tabs. Existing tabs keep their shell.",
                  )
          }
        >
          <Select
            value={terminalShell || SHELL_AUTO}
            onValueChange={(v) =>
              void setTerminalShell(v === SHELL_AUTO ? "" : v)
            }
          >
            <SelectTrigger
              value={terminalShell || SHELL_AUTO}
              className="h-8 w-40 text-ui-base"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={SHELL_AUTO} className="text-ui-base">
                {tr("Auto")}
              </SelectItem>
              {shells.map((s) => (
                <SelectItem
                  key={s.path}
                  value={s.path}
                  className="text-ui-base"
                >
                  {s.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingRow>
        {(wslDistros.length > 0 || defaultWorkspaceEnv !== "local") && (
          <SettingRow
            title={tr("Workspace environment")}
            description={tr(
              "Where new spaces run, terminal and AI agent alike: Windows or a WSL distro. Existing spaces keep theirs; switch any from the status bar.",
            )}
          >
            <Select
              value={defaultWorkspaceEnv}
              onValueChange={(v) => void setDefaultWorkspaceEnv(v)}
            >
              <SelectTrigger
                value={defaultWorkspaceEnv}
                className="h-8 w-40 text-ui-base"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="local" className="text-ui-base">
                  {tr("Windows")}
                </SelectItem>
                {wslDistros.map((d) => (
                  <SelectItem
                    key={d.name}
                    value={`wsl:${d.name}`}
                    className="text-ui-base"
                  >
                    {tr("WSL:")} {d.name}
                  </SelectItem>
                ))}
                {defaultWorkspaceEnv.startsWith("wsl:") &&
                  !wslDistros.some(
                    (d) => `wsl:${d.name}` === defaultWorkspaceEnv,
                  ) && (
                    <SelectItem
                      value={defaultWorkspaceEnv}
                      className="text-ui-base"
                    >
                      {defaultWorkspaceEnv.slice("wsl:".length)}{" "}
                      {tr("(unavailable)")}
                    </SelectItem>
                  )}
              </SelectContent>
            </Select>
          </SettingRow>
        )}
        <SettingRow
          title={tr("Letter spacing")}
          description={tr(
            "Extra horizontal space between characters (px). Use negative values to tighten Nerd Fonts.",
          )}
        >
          <Select
            value={String(terminalLetterSpacing)}
            onValueChange={(v) => void setTerminalLetterSpacing(Number(v))}
          >
            <SelectTrigger size="sm" className="h-8 w-28 text-ui-base">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {LETTER_SPACINGS.map((v) => (
                <SelectItem key={v} value={String(v)} className="text-ui-base">
                  {v > 0 ? `+${v}` : v} {tr("px")}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingRow>
        <SettingRow
          title={tr("Font size")}
          description={tr("Terminal text size.")}
        >
          <Select
            value={String(terminalFontSize)}
            onValueChange={(v) => void setTerminalFontSize(Number(v))}
          >
            <SelectTrigger size="sm" className="h-8 w-28 text-ui-base">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {TERMINAL_FONT_SIZES.map((size) => (
                <SelectItem
                  key={size}
                  value={String(size)}
                  className="text-ui-base"
                >
                  {size} {tr("px")}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingRow>
        <SettingRow
          title={tr("Scrollback")}
          description={tr(
            "Lines of history kept per terminal. Higher uses more RAM (~3 KB / line).",
          )}
        >
          <Select
            value={String(terminalScrollback)}
            onValueChange={(v) => void setTerminalScrollback(Number(v))}
          >
            <SelectTrigger size="sm" className="h-8 w-36 text-ui-base">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {TERMINAL_SCROLLBACK_PRESETS.map((lines) => (
                <SelectItem
                  key={lines}
                  value={String(lines)}
                  className="text-ui-base"
                >
                  {lines.toLocaleString()} {tr("lines")}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingRow>
        <SettingRow
          title={tr("Confirm before killing a running process")}
          description={tr(
            "Ask before closing a terminal tab or quitting while a command is still running. Unsaved editor changes are always confirmed.",
          )}
        >
          <Switch
            checked={confirmCloseRunningTerminal}
            onCheckedChange={(v) => void setConfirmCloseRunningTerminal(v)}
          />
        </SettingRow>
      </SettingGroup>

      <SettingGroup title={tr("Agents")}>
        <SettingRow
          title={tr("Coding agent notifications")}
          description={tr(
            "Alert when a coding agent needs your input or finishes. Native notification when RCode is unfocused, in-app otherwise.",
          )}
        >
          <div className="flex items-center gap-2">
            <Button
              type="button"
              variant="outline"
              size="xs"
              disabled={!agentNotifications || notificationTestPending}
              title={notificationTestTitle(notificationTest, tr)}
              onClick={() => void testNotification()}
            >
              {notificationTestLabel(notificationTest, tr)}
            </Button>
            <Switch
              checked={agentNotifications}
              disabled={notificationTestPending}
              onCheckedChange={(v) => {
                setNotificationTest("idle");
                void setAgentNotifications(v);
              }}
            />
          </div>
        </SettingRow>
        <SettingRow
          title={tr("Notification sound")}
          description={tr(
            "Play a sound with agent notifications and in-app alerts.",
          )}
        >
          <Switch
            checked={agentNotificationSound}
            disabled={!agentNotifications || notificationTestPending}
            onCheckedChange={(v) => void setAgentNotificationSound(v)}
          />
        </SettingRow>
      </SettingGroup>

      <SettingGroup title={tr("Startup")}>
        <SettingRow
          title={tr("Launch at login")}
          description={tr("Open RCode automatically when you sign in.")}
        >
          <Switch
            checked={autostart}
            onCheckedChange={(v) => void onToggleAutostart(v)}
          />
        </SettingRow>
        <SettingRow
          title={tr("Restore window position & size")}
          description={tr(
            "Reopen the main window where you left it. Applies on next launch.",
          )}
        >
          <Switch
            checked={restoreWindowState}
            onCheckedChange={(v) => void setRestoreWindowState(v)}
          />
        </SettingRow>
      </SettingGroup>
    </div>
  );
}

function Label({ children }: { children: React.ReactNode }) {
  return (
    <span className="text-ui-base font-medium tracking-tight text-muted-foreground">
      {children}
    </span>
  );
}

function notificationTestLabel(
  status: NotificationTestState,
  tr: ReturnType<typeof useTranslation>,
): string {
  switch (status) {
    case "waiting":
      return tr("Switch apps...");
    case "sending":
      return tr("Sending...");
    case "requested":
      return tr("Requested");
    case "denied":
      return tr("Blocked");
    case "failed":
      return tr("Failed");
    default:
      return tr("Test in 2s");
  }
}

function notificationTestTitle(
  status: NotificationTestState,
  tr: ReturnType<typeof useTranslation>,
): string {
  switch (status) {
    case "waiting":
      return tr("Switch to another app to verify native delivery");
    case "requested":
      return tr("The native notification was requested");
    case "denied":
      return tr("Notifications are disabled by the system");
    case "failed":
      return tr("RCode could not request a native notification");
    default:
      return tr("Send a native test notification after two seconds");
  }
}

function FontFamilyInput({
  value,
  onCommit,
}: {
  value: string;
  onCommit: (v: string) => void;
}) {
  const tr = useTranslation();
  const [draft, setDraft] = useState(value);

  useEffect(() => {
    setDraft(value);
  }, [value]);

  // Commit (and trim) only on blur/Enter so a trailing space can be typed
  // mid-edit, e.g. "JetBrains Mono ".
  const commit = () => {
    const next = draft.trim();
    if (next !== draft) setDraft(next);
    if (next !== value) onCommit(next);
  };

  return (
    <SettingRow
      title={tr("Font family")}
      description={tr(
        'Nerd Font name for icons (e.g. "CaskaydiaCove Nerd Font Mono"). Leave blank to auto-detect.',
      )}
    >
      <input
        type="text"
        value={draft}
        placeholder={tr("Auto-detect")}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
        }}
        className="h-8 w-48 rounded-md border border-border bg-background px-2.5 text-ui-base outline-none focus:border-foreground/40"
      />
    </SettingRow>
  );
}

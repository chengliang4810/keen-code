import { cachedRead, invalidateReadCache } from "@/lib/readCache";
import { useCallback, useEffect, useMemo, useState } from "react";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NumberField } from "@appica/ui-react/number-field";
import { Button } from "@appica/ui-react/button";
import { Switch } from "@/components/ui/switch";
import { IconPlay, IconRefresh, IconStop } from "@/components/icons";
import { createT, type Locale } from "@/i18n";
import * as api from "@/lib/api";

const MIN_WEB_HOST_PORT = 1;
const MAX_WEB_HOST_PORT = 65_535;
const MIN_WEB_HOST_TOKEN_BYTES = 8;
const MAX_WEB_HOST_TOKEN_BYTES = 512;

type WebHostAction = "status" | "start" | "stop" | "token" | null;

interface WebHostSettingsPanelProps {
  locale: Locale;
  settings: api.WebHostSettings;
  onSettingsChange: (settings: api.WebHostSettings) => void;
}

function isRunning(status: api.WebHostStatus | null): boolean {
  return status?.state === "running" || status?.state === "stopping";
}

function displayHost(bind: string): string {
  return bind.includes(":") && !bind.startsWith("[") ? `[${bind}]` : bind;
}

/** 与 Rust `WebToken` 保持一致，按 UTF-8 字节而不是 UTF-16 字符校验。 */
export function isValidWebHostToken(value: string): boolean {
  const bytes = new TextEncoder().encode(value).byteLength;
  return bytes >= MIN_WEB_HOST_TOKEN_BYTES &&
    bytes <= MAX_WEB_HOST_TOKEN_BYTES &&
    !/[\s\p{Cc}]/u.test(value);
}

/** Desktop Web Host 的配置和生命周期面板；Token 输入只存在于当前组件状态。 */
export function WebHostSettingsPanel({
  locale,
  settings,
  onSettingsChange,
}: WebHostSettingsPanelProps) {
  const t = useMemo(() => createT(locale), [locale]);
  const [status, setStatus] = useState<api.WebHostStatus | null>(null);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState<WebHostAction>(null);
  const [error, setError] = useState<string | null>(null);
  const [tokenSaved, setTokenSaved] = useState(false);

  const refreshStatus = useCallback(async () => {
    if (!api.isTauri()) return;
    setBusy("status");
    try {
      setStatus(await cachedRead("web_host_status", () => api.webHostStatus()));
      setError(null);
    } catch {
      setError(t("settings.webHost.statusError"));
    } finally {
      setBusy(null);
    }
  }, [t]);

  useEffect(() => {
    void refreshStatus();
  }, [refreshStatus]);

  const handleEnabledChange = useCallback(
    async (enabled: boolean) => {
      setError(null);
      if (!enabled && status?.state === "running") {
        setBusy("stop");
        try {
          invalidateReadCache("web_host_status");
          setStatus(await api.webHostStop());
        } catch {
          setError(t("settings.webHost.stopError"));
          return;
        } finally {
          setBusy(null);
        }
      }
      onSettingsChange({ ...settings, enabled });
    },
    [onSettingsChange, settings, status, t],
  );

  const runLifecycleAction = useCallback(
    async (action: "start" | "stop") => {
      setBusy(action);
      setError(null);
      try {
        const next = action === "start"
          ? await api.webHostStart(settings.port)
          : await api.webHostStop();
      invalidateReadCache("web_host_status");
        setStatus(next);
      } catch {
        setError(t(action === "start" ? "settings.webHost.startError" : "settings.webHost.stopError"));
      } finally {
        setBusy(null);
      }
    },
    [settings.port, t],
  );

  const saveToken = useCallback(async () => {
    const value = token;
    if (!isValidWebHostToken(value)) {
      setError(t("settings.webHost.tokenInvalid"));
      return;
    }
    setBusy("token");
    setError(null);
    try {
      invalidateReadCache("web_host_status");
      setStatus(await api.webHostSetToken(value));
      setToken("");
      setTokenSaved(true);
    } catch {
      setError(t("settings.webHost.tokenError"));
    } finally {
      setBusy(null);
    }
  }, [t, token]);

  const statusLabel = status
    ? t(`settings.webHost.state.${status.state}` as Parameters<typeof t>[0])
    : t("settings.webHost.state.unknown");
  const running = isRunning(status);
  const canStart = settings.enabled && status?.state !== "running" && status?.state !== "stopping";
  const statusUrl = status && status.state === "running"
    ? `http://${displayHost(status.bind)}:${status.port}`
    : null;

  return (
    <Card className="web-host-settings">
      <div className="settings-row">
        <div className="settings-row__text">
          <div className="settings-row__label">{t("settings.webHost.enabled")}</div>
          <div className="settings-row__desc">{t("settings.webHost.enabledDesc")}</div>
        </div>
        <Switch
          checked={settings.enabled}
          size="md"
          disabled={busy !== null}
          aria-label={t("settings.webHost.enabled")}
          title={t("settings.webHost.enabled")}
          onCheckedChange={(value) => void handleEnabledChange(value === true)}
        />
      </div>
      <div className="settings-row settings-row--stack">
        <div className="settings-row__text">
          <label className="settings-row__label" htmlFor="settings-web-host-bind">
            {t("settings.webHost.bind")}
          </label>
          <div className="settings-row__desc" id="settings-web-host-bind-desc">
            {t("settings.webHost.bindDesc")}
          </div>
        </div>
        <Input
          key={settings.bind}
          id="settings-web-host-bind"
          defaultValue={settings.bind}
          disabled={busy !== null || running}
          aria-describedby="settings-web-host-bind-desc"
          onBlur={(event) => {
            const value = event.currentTarget.value.trim();
            if (value && value !== settings.bind) {
              onSettingsChange({ ...settings, bind: value });
            }
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
          }}
        />
      </div>
      <div className="settings-row settings-row--stack">
        <div className="settings-row__text">
          <label className="settings-row__label" htmlFor="settings-web-host-port">
            {t("settings.webHost.port")}
          </label>
          <div className="settings-row__desc" id="settings-web-host-port-desc">
            {t("settings.webHost.portDesc")}
          </div>
        </div>
        <NumberField
          key={settings.port}
          id="settings-web-host-port"
          size="md"
          min={MIN_WEB_HOST_PORT}
          max={MAX_WEB_HOST_PORT}
          step={1}
          defaultValue={settings.port}
          disabled={busy !== null}
          aria-describedby="settings-web-host-port-desc"
          onValueCommitted={(value) => {
            if (
              value == null ||
              !Number.isInteger(value) ||
              value < MIN_WEB_HOST_PORT ||
              value > MAX_WEB_HOST_PORT ||
              value === settings.port
            ) return;
            onSettingsChange({ ...settings, port: value });
          }}
        />
      </div>
      <div className="settings-row settings-row--stack">
        <div className="settings-row__text">
          <div className="settings-row__label">{t("settings.webHost.status")}</div>
          <div className="settings-row__desc">{t("settings.webHost.statusDesc")}</div>
        </div>
        <div
          className="web-host-settings__status"
          data-state={status?.state ?? "unknown"}
          aria-live="polite"
        >
          <span className="web-host-settings__status-dot" aria-hidden="true" />
          <span>{statusLabel}</span>
          {statusUrl ? <code className="web-host-settings__url">{statusUrl}</code> : null}
          {status?.activeConnections ? (
            <span className="web-host-settings__connections">
              {t("settings.webHost.connections", { count: status.activeConnections })}
            </span>
          ) : null}
          <Button
            type="button"
            size="md"
            variant={running ? "ghost" : "primary"}
            disabled={busy !== null || (!running && !canStart)}
            onClick={() => void runLifecycleAction(running ? "stop" : "start")}
          >
            {running ? <IconStop size={15} /> : <IconPlay size={15} />}
            {busy === "start"
              ? t("settings.webHost.starting")
              : busy === "stop"
                ? t("settings.webHost.stopping")
                : running
                  ? t("settings.webHost.stop")
                  : t("settings.webHost.start")}
          </Button>
          <Button
            type="button"
            size="md"
            variant="ghost"
            disabled={busy !== null}
            aria-label={t("settings.webHost.refresh")}
            title={t("settings.webHost.refresh")}
            onClick={() => void refreshStatus()}
          >
            <IconRefresh size={15} />
          </Button>
        </div>
      </div>
      <div className="settings-row settings-row--stack">
        <div className="settings-row__text">
          <label className="settings-row__label" htmlFor="settings-web-host-token">
            {t("settings.webHost.token")}
          </label>
          <div className="settings-row__desc" id="settings-web-host-token-desc">
            {t("settings.webHost.tokenDesc")}
          </div>
        </div>
        <div className="web-host-settings__token">
          <Input
            id="settings-web-host-token"
            type="password"
            value={token}
            maxLength={512}
            autoComplete="new-password"
            autoCapitalize="none"
            spellCheck={false}
            disabled={busy !== null}
            placeholder={t("settings.webHost.tokenPlaceholder")}
            aria-describedby="settings-web-host-token-desc"
            onChange={(event) => {
              setToken(event.target.value);
              setTokenSaved(false);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") void saveToken();
            }}
          />
          <Button
            type="button"
            variant="primary"
            size="md"
            disabled={busy !== null || token.length === 0}
            onClick={() => void saveToken()}
          >
            {busy === "token" ? t("settings.webHost.tokenSaving") : t("settings.webHost.tokenSave")}
          </Button>
          {tokenSaved ? (
            <span className="web-host-settings__saved" aria-live="polite">
              {t("settings.webHost.tokenSaved")}
            </span>
          ) : null}
        </div>
      </div>
      {status?.tokenVersion && status.tokenVersion > 0 ? (
        <div className="web-host-settings__token-version">
          {t("settings.webHost.tokenVersion", { version: status.tokenVersion })}
        </div>
      ) : null}
      {error ? <div className="web-host-settings__error" role="alert">{error}</div> : null}
    </Card>
  );
}

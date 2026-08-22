import { Button } from "@/components/ui/button";
import { useMemo } from "react";
import { Spinner } from "@/components/ui/spinner";
import { createT, type Locale } from "@/i18n";
import type { AppUpdateStatus } from "@/lib/api";
import {
  appUpdateProgressPercent,
  formatUpdateBytes,
} from "@/lib/appUpdate";

export interface AppUpdateProgressProps {
  locale: Locale;
  status: AppUpdateStatus | null;
  installing: boolean;
  error: string | null;
  onRetry: () => void | Promise<void>;
  onInstall: () => void;
}

/** 更新弹窗内容；关闭弹窗不会中断后端下载。 */
export function AppUpdateProgress({
  locale,
  status,
  installing,
  error,
  onRetry,
  onInstall,
}: AppUpdateProgressProps) {
  const t = useMemo(() => createT(locale), [locale]);
  const state = installing ? "installing" : (status?.downloadState ?? "idle");
  const version = status?.latestRelease ?? status?.latestVersion ?? "";
  const percent = appUpdateProgressPercent(status);
  const downloaded = formatUpdateBytes(status?.downloadedBytes ?? 0, locale);
  const total = status?.totalBytes
    ? formatUpdateBytes(status.totalBytes, locale)
    : null;
  const failure = error ?? status?.downloadError;
  const source = status?.downloadSource === "chinaMirror"
    ? t("settings.updateSourceChinaMirror")
    : status?.downloadSource === "github"
      ? t("settings.updateSourceGithub")
      : null;

  let message = t("settings.updateAvailable", { version });
  if (state === "downloading") {
    message = t("settings.updateDownloading", { version });
  } else if (state === "verifying") {
    message = t("settings.updateVerifying");
  } else if (state === "ready") {
    message = t("settings.updateReady", { version });
  } else if (state === "installing") {
    message = t("settings.updateInstalling");
  } else if (state === "failed") {
    message = t("settings.updateDownloadFailed", { version });
  }

  const showDownloadProgress = state === "downloading" || state === "verifying";
  const showSpinner =
    state === "verifying" || state === "installing" ||
    (state === "downloading" && percent === null);

  return (
    <div className="app-update-progress" role="status" aria-live="polite">
      <div className="app-update-progress__status">
        {showSpinner ? <Spinner size={18} /> : null}
        <span>{message}</span>
      </div>
      {source ? (
        <div className="app-update-progress__source">
          {t("settings.updateCurrentSource", { source })}
        </div>
      ) : null}

      {showDownloadProgress ? (
        <>
          <div
            className={`app-update-progress__track${percent === null ? " is-indeterminate" : ""}`}
            role="progressbar"
            aria-label={t("settings.updateDownloadProgress")}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={percent ?? undefined}
          >
            <div
              className="app-update-progress__bar"
              style={percent === null ? undefined : { width: `${percent}%` }}
            />
          </div>
          <div className="app-update-progress__detail">
            {total && percent !== null
              ? t("settings.updateProgressKnown", {
                  downloaded,
                  total,
                  percent: String(percent),
                })
              : t("settings.updateProgressUnknown", { downloaded })}
          </div>
          <p className="app-update-progress__hint">
            {t("settings.updateBackgroundHint")}
          </p>
        </>
      ) : null}

      {failure ? (
        <div className="app-update-progress__error" role="alert">
          {failure}
        </div>
      ) : null}

      {state === "failed" || state === "ready" ? (
        <div className="app-update-progress__actions modal-actions">
          {state === "failed" ? (
            <Button
              type="button"
              className="btn btn--solid"
              onClick={() => void onRetry()}
            >
              {t("settings.updateRetry")}
            </Button>
          ) : (
            <Button type="button" className="btn btn--solid" onClick={onInstall}>
              {t("settings.updateInstall")}
            </Button>
          )}
        </div>
      ) : null}
    </div>
  );
}

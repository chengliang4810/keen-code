import { useMemo } from "react";
import { createT, type Locale } from "@/i18n";
import type { AppUpdateStatus } from "@/lib/api";

export type AppUpdateBusy = "checking" | "installing" | null;

export interface AppUpdateSectionProps {
  locale: Locale;
  status: AppUpdateStatus | null;
  busy: AppUpdateBusy;
  error: string | null;
  onCheck: () => void | Promise<void>;
  onInstall: () => void | Promise<void>;
}

/** “关于”页中的手动更新入口；自动检查由应用根组件统一触发。 */
export function AppUpdateSection({
  locale,
  status,
  busy,
  error,
  onCheck,
  onInstall,
}: AppUpdateSectionProps) {
  const t = useMemo(() => createT(locale), [locale]);
  const updateAvailable = status?.available === true;
  const latestRelease = status?.latestRelease ?? status?.latestVersion ?? "";

  let description = t("settings.updateIdle");
  if (busy === "checking") description = t("settings.updateChecking");
  else if (busy === "installing") description = t("settings.updateInstalling");
  else if (updateAvailable) {
    description = t("settings.updateAvailable", { version: latestRelease });
  } else if (status?.checked) description = t("settings.updateCurrent");

  return (
    <div className="settings-about__update">
      <div className="settings-about__update-copy">
        <div className="settings-about__update-title">
          {t("settings.updateTitle")}
        </div>
        <div className="settings-row__desc">{description}</div>
        {error ? (
          <div className="settings-about__update-error" role="alert">
            {error}
          </div>
        ) : null}
        {updateAvailable && status?.notes ? (
          <details className="settings-about__update-notes">
            <summary>{t("settings.updateNotes")}</summary>
            <p>{status.notes}</p>
          </details>
        ) : null}
      </div>
      <button
        type="button"
        className={`btn ${updateAvailable ? "btn--solid" : "btn--ghost"} btn--sm`}
        disabled={busy !== null}
        onClick={() => {
          void (updateAvailable ? onInstall() : onCheck());
        }}
      >
        {busy === "checking"
          ? t("settings.updateCheckingAction")
          : busy === "installing"
            ? t("settings.updateInstallingAction")
            : updateAvailable
              ? t("settings.updateInstall")
              : t("settings.updateCheck")}
      </button>
    </div>
  );
}

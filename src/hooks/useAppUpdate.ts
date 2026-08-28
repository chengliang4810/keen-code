import { useCallback, useEffect, useState } from "react";
import * as api from "@/lib/api";
import { localizeUiError } from "@/lib/session";
import type { Locale } from "@/i18n";

const CHECK_INTERVAL_MS = 30 * 60 * 1000;
let checkInFlight: Promise<api.AppUpdateStatus> | null = null;

async function checkForUpdate() {
  const request = checkInFlight ??= api.appUpdateCheck();
  try {
    return await request;
  } finally {
    if (checkInFlight === request) checkInFlight = null;
  }
}

export function useAppUpdate(appBooting: boolean, locale: Locale) {
  const [status, setStatus] = useState<api.AppUpdateStatus | null>(null);
  const [busy, setBusy] = useState<"checking" | "installing" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [progressOpen, setProgressOpen] = useState(false);

  useEffect(() => {
    if (!api.isTauri()) return;
    let active = true;
    let unlisten: (() => void) | undefined;
    void api
      .listen<api.AppUpdateStatus>(api.APP_UPDATE_STATUS_EVENT, (next) => {
        if (!active) return;
        setStatus(next);
        if (next.downloadState !== "failed") setError(null);
      })
      .then((stop) => (active ? (unlisten = stop) : stop()))
      .catch(() => {});
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  const install = useCallback(async () => {
    if (!api.isTauri()) return;
    setProgressOpen(true);
    setBusy("installing");
    setError(null);
    try {
      await api.appUpdateInstall();
    } catch (cause) {
      setError(localizeUiError(cause, locale));
      void api.appUpdateInfo().then(setStatus).catch(() => {});
    } finally {
      setBusy(null);
    }
  }, [locale]);

  const check = useCallback(async () => {
    if (!api.isTauri()) return;
    setBusy("checking");
    setError(null);
    try {
      setStatus(await checkForUpdate());
    } catch (cause) {
      setError(localizeUiError(cause, locale));
    } finally {
      setBusy(null);
    }
  }, [locale]);

  useEffect(() => {
    if (appBooting || !api.isTauri()) return;
    let active = true;
    void api
      .appUpdateInfo()
      .then((next) => active && setStatus((current) => current ?? next))
      .catch(() => {});
    const silentCheck = () => {
      void checkForUpdate().then((next) => active && setStatus(next)).catch(() => {});
    };
    silentCheck();
    const timer = window.setInterval(silentCheck, CHECK_INTERVAL_MS);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [appBooting]);

  return {
    status,
    busy,
    error,
    progressOpen,
    setProgressOpen,
    check,
    install,
  };
}

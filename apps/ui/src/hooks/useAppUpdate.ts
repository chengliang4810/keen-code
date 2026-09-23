import { useCallback, useEffect, useState } from "react";
import * as api from "@/lib/api";
import { localizeUiError } from "@/lib/session";
import type { Locale } from "@/i18n";

const CHECK_INTERVAL_MS = 30 * 60 * 1000;
/** 启动后首次静默检查的延迟：避开启动命令高峰，先让界面可交互。 */
const FIRST_CHECK_DELAY_MS = 15 * 1000;
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
    // 本地构建信息读取仍然立即进行（不访问网络）。
    void api
      .appUpdateInfo()
      .then((next) => active && setStatus((current) => current ?? next))
      .catch(() => {});
    const silentCheck = () => {
      void checkForUpdate().then((next) => active && setStatus(next)).catch(() => {});
    };
    // 启动后延迟首次网络检查：更新检查会访问远端清单（实测约 1.7 秒），
    // 与启动路径上的其他命令并发时拖慢首个可交互窗口。等界面稳定后再跑，
    // 用户手动检查（check()）不受影响。
    const firstCheckDelay = window.setTimeout(silentCheck, FIRST_CHECK_DELAY_MS);
    const timer = window.setInterval(silentCheck, CHECK_INTERVAL_MS);
    return () => {
      active = false;
      window.clearTimeout(firstCheckDelay);
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

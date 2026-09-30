import { useCallback, useEffect, useRef, useState } from "react";
import QRCode from "qrcode";
import { Badge } from "@appica/ui-react/badge";
import { invalidateReadCache } from "@/lib/readCache";
import { buildMobileRemoteUrl, isLoopbackBind } from "@/lib/webHostUrl";
import * as api from "@/lib/api";
import { GlassModal } from "@/components/GlassModal";
import { Button } from "@/components/ui/button";
import {
  IconCopy,
  IconQrcode,
  IconRefresh,
} from "@/components/icons";
import type { MessageKey } from "@/i18n";
import type { SetState, Translator } from "./types";

export interface RemoteControlModalProps {
  tr: Translator;
  open: boolean;
  setOpen: SetState<boolean>;
  onOpenSettings: () => void;
}

interface RemoteControlModalViewProps {
  tr: Translator;
  status: api.WebHostStatus | null;
  shareUrl: string | null;
  qrDataUrl: string | null;
  busy: boolean;
  copied: boolean;
  error: string | null;
  onToggleRun: () => void;
  onRefresh: () => void;
  onCopy: () => void;
  onOpenSettings: () => void;
}

/**
 * 左下角「手机远程连接」配对弹窗。容器只负责 Web Host 状态/Token 的读取与
 * 启停动作；配对 URL 由 lib/webHostUrl 统一构造，视图保持纯渲染便于测试。
 */
export function RemoteControlModal({
  tr,
  open,
  setOpen,
  onOpenSettings,
}: RemoteControlModalProps) {
  const [status, setStatus] = useState<api.WebHostStatus | null>(null);
  const [token, setToken] = useState("");
  const [qrDataUrl, setQrDataUrl] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trRef = useRef(tr);
  const copyTimerRef = useRef<number | null>(null);

  useEffect(() => {
    trRef.current = tr;
  }, [tr]);

  const load = useCallback(async () => {
    if (!api.isTauri()) return;
    setBusy(true);
    try {
      const [nextStatus, nextToken] = await Promise.all([
        api.webHostStatus(),
        api.webHostGetToken(),
      ]);
      setStatus(nextStatus);
      setToken(nextToken);
      setError(null);
    } catch {
      setError(trRef.current("remoteControl.loadError"));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    if (open) void load();
  }, [open, load]);

  const running = status?.state === "running";
  const shareUrl = status && running && !isLoopbackBind(status.bind)
    ? buildMobileRemoteUrl(status.bind, status.port, token)
    : null;

  useEffect(() => {
    if (!shareUrl) {
      setQrDataUrl(null);
      return;
    }
    let active = true;
    void QRCode.toDataURL(shareUrl, { errorCorrectionLevel: "M", margin: 2, width: 480 })
      .then((url) => {
        if (active) setQrDataUrl(url);
      })
      .catch(() => {
        if (active) setQrDataUrl(null);
      });
    return () => {
      active = false;
    };
  }, [shareUrl]);

  useEffect(() => () => {
    if (copyTimerRef.current !== null) window.clearTimeout(copyTimerRef.current);
  }, []);

  const toggleRun = useCallback(async () => {
    setBusy(true);
    try {
      const next = running ? await api.webHostStop() : await api.webHostStart();
      invalidateReadCache("web_host_status");
      setStatus(next);
    } catch (cause) {
      const detail =
        typeof cause === "string" ? cause.trim() : cause instanceof Error ? cause.message.trim() : "";
      setError(
        detail
          ? trRef.current("remoteControl.actionFailed", { error: detail })
          : trRef.current("remoteControl.loadError"),
      );
    } finally {
      setBusy(false);
    }
  }, [running]);

  const copy = useCallback(async () => {
    if (!shareUrl) return;
    try {
      await navigator.clipboard.writeText(shareUrl);
      setCopied(true);
      if (copyTimerRef.current !== null) window.clearTimeout(copyTimerRef.current);
      copyTimerRef.current = window.setTimeout(() => setCopied(false), 2000);
    } catch {
      setError(trRef.current("remoteControl.copyFailed"));
    }
  }, [shareUrl]);

  return (
    <GlassModal
      open={open}
      onClose={() => setOpen(false)}
      title={tr("remoteControl.title")}
      size="md"
      closeLabel={tr("remoteControl.close")}
    >
      <RemoteControlModalView
        tr={tr}
        status={status}
        shareUrl={shareUrl}
        qrDataUrl={qrDataUrl}
        busy={busy}
        copied={copied}
        error={error}
        onToggleRun={() => void toggleRun()}
        onRefresh={() => void load()}
        onCopy={() => void copy()}
        onOpenSettings={onOpenSettings}
      />
    </GlassModal>
  );
}

/** 纯视图：按状态渲染扫码面板，静态渲染即可覆盖各状态分支。 */
export function RemoteControlModalView({
  tr,
  status,
  shareUrl,
  qrDataUrl,
  busy,
  copied,
  error,
  onToggleRun,
  onRefresh,
  onCopy,
  onOpenSettings,
}: RemoteControlModalViewProps) {
  const running = status?.state === "running";
  const loopback = Boolean(status && running && isLoopbackBind(status.bind));
  const stateLabel = !status
    ? tr("remoteControl.state.loading")
    : running
      ? tr("remoteControl.state.waiting")
      : tr(`settings.webHost.state.${status.state}` as MessageKey);

  return (
    <div className="remote-control" data-status={status?.state ?? "unknown"}>
      <p className="remote-control__desc">{tr("remoteControl.description")}</p>
      <section className="remote-control__panel" aria-busy={busy || undefined}>
        <div className="remote-control__panel-head">
          <span className="remote-control__panel-icon" aria-hidden="true">
            <IconQrcode size={20} />
          </span>
          <div className="remote-control__panel-heading">
            <h3 className="remote-control__panel-title">
              {tr("remoteControl.section.title")}
            </h3>
            <p className="remote-control__panel-desc">
              {tr("remoteControl.section.description")}
            </p>
          </div>
        </div>
        <div className="remote-control__status" data-state={status?.state ?? "unknown"}>
          <div className="remote-control__status-text">
            <div className="remote-control__status-line">
              <span className="remote-control__status-dot" aria-hidden="true" />
              <span>{stateLabel}</span>
              {running ? <Badge size="md" variant="success">{tr("remoteControl.state.ready")}</Badge> : null}
            </div>
            <p className="remote-control__status-hint">
              {running ? tr("remoteControl.hint.scan") : tr("remoteControl.hint.offline")}
            </p>
          </div>
          <Button
            type="button"
            size="md"
            variant="ghost"
            disabled={busy}
            onClick={onToggleRun}
          >
            {running ? tr("remoteControl.action.stop") : tr("remoteControl.action.start")}
          </Button>
        </div>
        {loopback ? (
          <div className="remote-control__warning" role="status">
            <p>{tr("remoteControl.loopbackWarning")}</p>
            <Button
              type="button"
              size="md"
              variant="outline"
              onClick={onOpenSettings}
            >
              {tr("remoteControl.action.openSettings")}
            </Button>
          </div>
        ) : null}
        {shareUrl ? (
          <>
            <div className="remote-control__urlbar">
              <p className="remote-control__urlbar-hint">{tr("remoteControl.hint.noQr")}</p>
              <div className="remote-control__urlbar-actions">
                <Button
                  type="button"
                  size="md"
                  variant="ghost"
                  disabled={busy}
                  onClick={onRefresh}
                >
                  <IconRefresh size={15} />
                  <span>{tr("remoteControl.action.refreshQr")}</span>
                </Button>
                <Button
                  type="button"
                  size="md"
                  variant="ghost"
                  onClick={onCopy}
                >
                  <IconCopy size={15} />
                  <span>
                    {copied
                      ? tr("remoteControl.copied")
                      : tr("remoteControl.action.copyLink")}
                  </span>
                </Button>
              </div>
            </div>
            <div className="remote-control__qr">
              {qrDataUrl ? (
                <img
                  className="remote-control__qr-image"
                  src={qrDataUrl}
                  alt={tr("remoteControl.qrAlt")}
                />
              ) : (
                <div className="remote-control__qr-loading" aria-hidden="true" />
              )}
            </div>
          </>
        ) : null}
      </section>
      {error ? <p className="remote-control__error" role="alert">{error}</p> : null}
    </div>
  );
}

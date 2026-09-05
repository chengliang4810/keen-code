import { Button } from "@/components/ui/button";
/**
 * Built-in browser for the resource pane.
 *
 * Plain <iframe> is blocked by X-Frame-Options / CSP on many sites (GitHub, etc.)
 * → blank preview. In Tauri we attach a child native Webview over this host
 * element so the page loads as a top-level document.
 *
 * Non-Tauri (dev UI only): falls back to iframe + open-external affordance.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { isTauri, urlOpen } from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import { localizeUiError } from "@/lib/session";
import { IconExternalLink, IconRefresh } from "@/components/icons";
import type { Webview } from "@tauri-apps/api/webview";

const WEBVIEW_LABEL = "resource-browser";

export interface EmbeddedBrowserProps {
  url: string;
  title?: string;
  locale?: Locale;
  /** When false, native webview is hidden (inactive tab / collapsed pane). */
  active?: boolean;
  className?: string;
}

function sanitizeLabel(s: string): string {
  return s.replace(/[^a-zA-Z0-9\-_:/]/g, "-").slice(0, 64) || "resource-browser";
}

async function openExternalUrl(url: string) {
  if (isTauri()) {
    await urlOpen(url);
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

/** 在 Tauri 中托管原生 Webview，并在普通浏览器环境提供明确降级界面。 */
export function EmbeddedBrowser({
  url,
  title,
  locale = "en",
  active = true,
  className = "",
}: EmbeddedBrowserProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  /** 动态导入创建的原生 Webview；类型导入不引入运行时依赖。 */
  const webviewRef = useRef<Webview | null>(null);
  const currentUrlRef = useRef<string>("");
  /** 供异步创建流程读取最新语言，避免语言切换时重建 Webview。 */
  const localeRef = useRef(locale);
  localeRef.current = locale;
  const [error, setError] = useState<string | null>(null);
  const [ready, setReady] = useState(false);
  const tr = createT(locale);

  /** 将宿主元素的最新布局同步到原生 Webview，并按激活状态控制显隐。 */
  const syncBounds = useCallback(async () => {
    const el = hostRef.current;
    const wv = webviewRef.current;
    if (!el || !wv || !isTauri()) return;
    const rect = el.getBoundingClientRect();
    if (rect.width < 2 || rect.height < 2) {
      try {
        await wv.hide();
      } catch {
        /* ignore */
      }
      return;
    }
    try {
      const { LogicalPosition, LogicalSize } = await import("@tauri-apps/api/dpi");
      await wv.setPosition(new LogicalPosition(rect.left, rect.top));
      await wv.setSize(new LogicalSize(rect.width, rect.height));
      if (active) await wv.show();
      else await wv.hide();
    } catch (e) {
      console.error("[EmbeddedBrowser] syncBounds", e);
    }
  }, [active]);

  // Create / recreate native webview when URL changes (Tauri only)
  useEffect(() => {
    if (!isTauri() || !active) return;
    const target = url.trim();
    if (!target) return;

    let cancelled = false;
    let resizeObs: ResizeObserver | null = null;
    let roFrame = 0;

    /** 为当前 URL 创建原生 Webview，并在异步边界检查组件是否已经卸载。 */
    const boot = async () => {
      setError(null);
      setReady(false);
      try {
        const { Webview } = await import("@tauri-apps/api/webview");
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const { LogicalPosition, LogicalSize } = await import("@tauri-apps/api/dpi");
        const win = getCurrentWindow();

        // Tear down previous instance (URL change or remount)
        const existing = await Webview.getByLabel(WEBVIEW_LABEL);
        if (existing) {
          try {
            await existing.close();
          } catch {
            /* ignore */
          }
        }
        webviewRef.current = null;
        currentUrlRef.current = "";

        if (cancelled) return;

        const el = hostRef.current;
        const rect = el?.getBoundingClientRect();
        const x = rect?.left ?? 0;
        const y = rect?.top ?? 0;
        const w = Math.max(rect?.width ?? 320, 40);
        const h = Math.max(rect?.height ?? 240, 40);

        const webview = new Webview(win, sanitizeLabel(WEBVIEW_LABEL), {
          url: target,
          x,
          y,
          width: w,
          height: h,
          focus: true,
          // Accept any remote page; child is a real top-level document
          acceptFirstMouse: true,
        });

        await new Promise<void>((resolve, reject) => {
          const t = window.setTimeout(
            () => reject(new Error("webview create timeout")),
            8000,
          );
          void webview.once("tauri://created", () => {
            window.clearTimeout(t);
            resolve();
          });
          void webview.once("tauri://error", (e) => {
            window.clearTimeout(t);
            reject(e.payload ?? e);
          });
        });

        if (cancelled) {
          try {
            await webview.close();
          } catch {
            /* ignore */
          }
          return;
        }

        webviewRef.current = webview;
        currentUrlRef.current = target;
        await webview.setPosition(new LogicalPosition(x, y));
        await webview.setSize(new LogicalSize(w, h));
        await webview.show();
        setReady(true);

        // Keep bounds aligned with the host pane; hide when host not visible
        // (aside collapsed, zero-size, covered).
        if (hostRef.current && typeof ResizeObserver !== "undefined") {
          resizeObs = new ResizeObserver(() => {
            cancelAnimationFrame(roFrame);
            roFrame = requestAnimationFrame(() => {
              void syncBounds();
            });
          });
          resizeObs.observe(hostRef.current);
        }
        if (hostRef.current && typeof IntersectionObserver !== "undefined") {
          const io = new IntersectionObserver(
            (entries) => {
              const vis = entries.some((e) => e.isIntersecting && e.intersectionRatio > 0.05);
              const wv = webviewRef.current;
              if (!wv) return;
              if (!vis || !active) void wv.hide().catch(() => undefined);
              else void syncBounds();
            },
            { threshold: [0, 0.05, 0.5, 1] },
          );
          io.observe(hostRef.current);
          // stash on resizeObs cleanup via disconnect of both
          (resizeObs as unknown as { _io?: IntersectionObserver })._io = io;
        }
        window.addEventListener("resize", syncBounds);
      } catch (e) {
        if (!cancelled) {
          console.error("[EmbeddedBrowser] create failed", e);
          setError(localizeUiError(e, localeRef.current));
          setReady(false);
        }
      }
    };

    void boot();

    return () => {
      cancelled = true;
      cancelAnimationFrame(roFrame);
      resizeObs?.disconnect();
      const io = (resizeObs as unknown as { _io?: IntersectionObserver } | null)?._io;
      io?.disconnect();
      window.removeEventListener("resize", syncBounds);
      const wv = webviewRef.current;
      webviewRef.current = null;
      currentUrlRef.current = "";
      if (wv) {
        void wv.close().catch(() => undefined);
      } else if (isTauri()) {
        void import("@tauri-apps/api/webview")
          .then(({ Webview }) => Webview.getByLabel(WEBVIEW_LABEL))
          .then((w) => w?.close())
          .catch(() => undefined);
      }
    };
  }, [url, active, syncBounds]);

  // Hide/show when active toggles without URL change
  useEffect(() => {
    const wv = webviewRef.current;
    if (!wv || !isTauri()) return;
    if (active) {
      void syncBounds().then(() => wv.show()).catch(() => undefined);
    } else {
      void wv.hide().catch(() => undefined);
    }
  }, [active, syncBounds]);

  const openExternal = () => {
    void openExternalUrl(url).catch((cause) => setError(localizeUiError(cause, locale)));
  };

  const reload = () => {
    // Force recreate by remounting effect: clear then set same url via key is parent job.
    // Local: close + recreate
    if (!isTauri()) return;
    const u = url;
    void (async () => {
      try {
        const { Webview } = await import("@tauri-apps/api/webview");
        const w = await Webview.getByLabel(WEBVIEW_LABEL);
        if (w) await w.close();
      } catch {
        /* ignore */
      }
      webviewRef.current = null;
      currentUrlRef.current = "";
      // Trigger effect by bumping a dummy state through recreating with same url —
      // parent should change key; as fallback re-run boot by toggling ready
      setReady(false);
      setError(null);
      // Manual recreate
      const el = hostRef.current;
      if (!el) return;
      try {
        const { Webview } = await import("@tauri-apps/api/webview");
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const { LogicalPosition, LogicalSize } = await import("@tauri-apps/api/dpi");
        const rect = el.getBoundingClientRect();
        const webview = new Webview(getCurrentWindow(), WEBVIEW_LABEL, {
          url: u,
          x: rect.left,
          y: rect.top,
          width: Math.max(rect.width, 40),
          height: Math.max(rect.height, 40),
          focus: true,
        });
        await new Promise<void>((resolve, reject) => {
          void webview.once("tauri://created", () => resolve());
          void webview.once("tauri://error", (e) => reject(e));
        });
        webviewRef.current = webview;
        await webview.setPosition(new LogicalPosition(rect.left, rect.top));
        await webview.setSize(
          new LogicalSize(Math.max(rect.width, 40), Math.max(rect.height, 40)),
        );
        await webview.show();
        setReady(true);
      } catch (e) {
        setError(localizeUiError(e, locale));
      }
    })();
  };

  // Non-Tauri: iframe (many sites blank — surface open external)
  if (!isTauri()) {
    return (
      <div className={"embedded-browser " + className}>
        <div className="embedded-browser__bar">
          <span className="embedded-browser__url" title={url}>
            {url}
          </span>
          <Button
            type="button"
            className="chrome-btn"
            onClick={openExternal}
            title={tr("resources.openExternal")}
          >
            <IconExternalLink size={14} />
          </Button>
        </div>
        <iframe
          className="rp-preview__frame rp-preview__frame--browser"
          title={title || url}
          src={url}
          referrerPolicy="no-referrer"
          allow="fullscreen"
        />
        <div className="embedded-browser__hint">
          {tr("resources.browserIframeHint")}
        </div>
      </div>
    );
  }

  return (
    <div className={"embedded-browser embedded-browser--native " + className}>
      <div className="embedded-browser__bar">
        <span className="embedded-browser__url" title={url}>
          {url}
        </span>
        <Button
          type="button"
          className="chrome-btn"
          onClick={reload}
          title={tr("resources.browserReload")}
        >
          <IconRefresh size={14} />
        </Button>
        <Button
          type="button"
          className="chrome-btn"
          onClick={openExternal}
          title={tr("resources.openExternal")}
        >
          <IconExternalLink size={14} />
        </Button>
      </div>
      {/* Host rectangle — native webview is painted on top of this area */}
      <div
        ref={hostRef}
        className="embedded-browser__host"
        data-ready={ready ? "1" : "0"}
        aria-label={title || url}
      >
        {error ? (
          <div className="rp-preview__msg" role="alert">
            <p>{tr("resources.browserFailed")}</p>
            <p className="embedded-browser__err">{error}</p>
            <Button type="button" className="btn btn--primary" onClick={openExternal}>
              {tr("resources.openExternal")}
            </Button>
          </div>
        ) : !ready ? (
          <div className="rp-preview__msg">{tr("resources.loading")}</div>
        ) : (
          <div className="embedded-browser__host-fill" aria-hidden />
        )}
      </div>
    </div>
  );
}

/**
 * 右侧资源面板的内置浏览器网页标签。
 *
 * 每个标签持有一个独立原生子 WebView：切换标签只切换显隐，页面状态（滚动位置、
 * 表单内容、前端路由）因此保留。子 WebView 由系统原生层绘制，会盖住主界面浮层，
 * 所以浮层出现或标签不活跃时必须隐藏。
 *
 * 地址栏的地址解析走 `@/lib/browserTabs`：网页地址在本标签内导航，本地路径交给
 * 资源面板的文件预览链路（本地 HTML 由 HtmlBrowser 以 srcDoc 渲染）。
 *
 * 非桌面环境（浏览器夹具）不创建子 WebView，只保留地址栏结构与提示。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@appica/ui-react/input";
import { Alert, AlertDescription } from "@appica/ui-react/alert";
import { Tip } from "@/components/ui/tooltip";
import {
  IconArrowLeft,
  IconArrowRight,
  IconExternalLink,
  IconRefresh,
  IconWorld,
} from "@/components/icons";
import { createT, type Locale } from "@/i18n";
import { isTauri, urlOpen, type BrowserRect } from "@/lib/api";
import { localizeUiError } from "@/lib/session";
import { parseBrowserAddress } from "@/lib/browserTabs";
import {
  browserHistory,
  closeBrowserWebview,
  hideBrowserWebview,
  navigateBrowserWebview,
  openBrowserWebview,
  reloadBrowserWebview,
  setBrowserBounds,
  showBrowserWebview,
} from "@/lib/browserWebview";
import { useCoveringOverlay } from "@/hooks/useCoveringOverlay";

/** 子 WebView 回写的状态事件载荷，与 Rust 侧 `BrowserState` 对应。 */
interface BrowserStatePayload {
  tabId: string;
  url: string;
  title: string | null;
  kind: "started" | "finished" | "title";
}

export interface EmbeddedBrowserProps {
  /** 网页标签标识；同时决定子 WebView label。 */
  tabId: string;
  /** 标签初始地址；后续导航由地址栏驱动，不随该值变化重建。 */
  url: string;
  title?: string;
  locale: Locale;
  /** 是否为当前可见标签；false 时隐藏子 WebView 但保留页面。 */
  active: boolean;
  /** 页面地址或标题变化时回写标签。 */
  onNavigated?: (url: string, title?: string) => void;
  /** 地址栏输入本地路径时交给资源面板的文件预览链路。 */
  onOpenPath?: (path: string) => void;
}

/** 宿主元素的窗口内容区坐标；不可见或尺寸过小时返回 null。 */
function hostRect(el: HTMLElement | null): BrowserRect | null {
  if (!el) return null;
  const rect = el.getBoundingClientRect();
  if (rect.width < 2 || rect.height < 2) return null;
  return {
    left: rect.left,
    top: rect.top,
    width: rect.width,
    height: rect.height,
  };
}

export function EmbeddedBrowser({
  tabId,
  url,
  title,
  locale,
  active,
  onNavigated,
  onOpenPath,
}: EmbeddedBrowserProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const hostRef = useRef<HTMLDivElement>(null);
  const [address, setAddress] = useState(url);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 用户正在编辑地址栏时，不让页面导航覆盖输入内容。 */
  const editingRef = useRef(false);
  /** 是否已为该标签创建原生子 WebView。 */
  const createdRef = useRef(false);
  /** 组件是否仍挂载；卸载后丢弃迟到的异步结果。 */
  const mountedRef = useRef(true);
  const currentUrlRef = useRef(url);
  const initialUrlRef = useRef(url);
  const onNavigatedRef = useRef(onNavigated);
  const onOpenPathRef = useRef(onOpenPath);
  onNavigatedRef.current = onNavigated;
  onOpenPathRef.current = onOpenPath;

  const desktop = isTauri();
  // 主界面浮层会盖住原生子 WebView，被遮挡时必须让位。
  const occluded = useCoveringOverlay(hostRef, desktop && active);
  const visible = desktop && active && !occluded;

  const reportError = useCallback(
    (cause: unknown) => {
      setError(localizeUiError(cause, locale));
    },
    [locale],
  );

  // 首次可见时才创建原生子 WebView：未激活的标签不占用 web 引擎进程，
  // 但一旦创建就常驻，切回标签时页面状态仍在。
  useEffect(() => {
    mountedRef.current = true;
    if (!desktop || !visible || createdRef.current) return;
    createdRef.current = true;
    void (async () => {
      try {
        const rect = hostRect(hostRef.current) ?? {
          left: 0,
          top: 0,
          width: 640,
          height: 480,
        };
        await openBrowserWebview(tabId, initialUrlRef.current, rect);
        if (!mountedRef.current) return;
        setReady(true);
        setError(null);
      } catch (cause) {
        // 创建失败时允许下次可见重试。
        createdRef.current = false;
        if (mountedRef.current) reportError(cause);
      }
    })();
  }, [desktop, visible, tabId, reportError]);

  // 组件卸载时释放该标签的原生子 WebView。
  //
  // 严格模式会先卸载再挂载：这里同时复位 createdRef，使重新挂载时重建，
  // 否则首次创建会被紧随其后的清理关闭，标签永久空白。
  useEffect(() => {
    if (!desktop) return;
    return () => {
      mountedRef.current = false;
      createdRef.current = false;
      setReady(false);
      void closeBrowserWebview(tabId).catch(() => undefined);
    };
  }, [desktop, tabId]);

  // 对齐宿主区域；不可见时隐藏，避免原生层残留。
  const applyBounds = useCallback(async () => {
    if (!desktop) return;
    const rect = hostRect(hostRef.current);
    if (!rect) {
      await hideBrowserWebview(tabId).catch(() => undefined);
      return;
    }
    await setBrowserBounds(tabId, rect).catch(() => undefined);
  }, [desktop, tabId]);

  useEffect(() => {
    if (!desktop) return;
    const el = hostRef.current;
    if (!el) return;
    let frame = 0;
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        void applyBounds();
      });
    };
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    observer?.observe(el);
    window.addEventListener("resize", schedule);
    return () => {
      cancelAnimationFrame(frame);
      observer?.disconnect();
      window.removeEventListener("resize", schedule);
    };
  }, [applyBounds, desktop]);

  // 显示/隐藏跟随标签激活状态与浮层遮挡。
  useEffect(() => {
    if (!desktop) return;
    if (!visible) {
      void hideBrowserWebview(tabId).catch(() => undefined);
      return;
    }
    void (async () => {
      const rect = hostRect(hostRef.current);
      if (rect) await setBrowserBounds(tabId, rect).catch(() => undefined);
      await showBrowserWebview(tabId).catch(() => undefined);
    })();
  }, [desktop, tabId, visible]);

  // 接收页面导航与标题回写。
  useEffect(() => {
    if (!desktop) return;
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      const stop = await listen<BrowserStatePayload>("browser://state", (event) => {
        const payload = event.payload;
        if (payload.tabId !== tabId) return;
        if (payload.url && payload.url !== "about:blank") {
          currentUrlRef.current = payload.url;
          if (!editingRef.current) setAddress(payload.url);
          if (payload.kind === "finished") onNavigatedRef.current?.(payload.url);
        }
        if (payload.title) {
          onNavigatedRef.current?.(currentUrlRef.current, payload.title);
        }
      });
      if (disposed) stop();
      else unlisten = stop;
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [desktop, tabId]);

  const run = useCallback(
    (op: Promise<void>) => {
      void op.then(
        () => setError(null),
        (cause) => reportError(cause),
      );
    },
    [reportError],
  );

  const submit = useCallback(() => {
    const parsed = parseBrowserAddress(address);
    if (!parsed) {
      setError(tr("resources.browserBadAddress"));
      return;
    }
    if (parsed.kind === "file") {
      onOpenPathRef.current?.(parsed.path);
      return;
    }
    setAddress(parsed.url);
    editingRef.current = false;
    if (parsed.url === currentUrlRef.current) {
      run(reloadBrowserWebview(tabId));
      return;
    }
    currentUrlRef.current = parsed.url;
    run(navigateBrowserWebview(tabId, parsed.url));
  }, [address, run, tabId, tr]);

  const openExternal = useCallback(() => {
    const target = currentUrlRef.current;
    if (!target || target === "about:blank") return;
    if (desktop) {
      run(urlOpen(target));
      return;
    }
    window.open(target, "_blank", "noopener,noreferrer");
  }, [desktop, run]);

  const addressBar = (
    <div className="embedded-browser__bar">
      <Tip label={tr("resources.browserBack")}>
        <Button
          type="button"
          variant="ghost" size="icon-md"
          disabled={!ready}
          onClick={() => run(browserHistory(tabId, "back"))}
        >
          <IconArrowLeft size={14} />
        </Button>
      </Tip>
      <Tip label={tr("resources.browserForward")}>
        <Button
          type="button"
          variant="ghost" size="icon-md"
          disabled={!ready}
          onClick={() => run(browserHistory(tabId, "forward"))}
        >
          <IconArrowRight size={14} />
        </Button>
      </Tip>
      <Tip label={tr("resources.browserReload")}>
        <Button
          type="button"
          variant="ghost" size="icon-md"
          disabled={!ready}
          onClick={() => run(reloadBrowserWebview(tabId))}
        >
          <IconRefresh size={14} />
        </Button>
      </Tip>
      <div className="embedded-browser__address">
        <IconWorld size={13} />
        <Input
          value={address === "about:blank" ? "" : address}
          placeholder={tr("resources.browserAddressPlaceholder")}
          aria-label={tr("resources.browserAddress")}
          spellCheck={false}
          autoComplete="off"
          onFocus={() => {
            editingRef.current = true;
          }}
          onChange={(event) => setAddress(event.target.value)}
          onKeyDown={(event) => {
            if (event.key !== "Enter" || event.nativeEvent.isComposing) return;
            event.preventDefault();
            submit();
          }}
          onBlur={() => {
            editingRef.current = false;
            setAddress(currentUrlRef.current);
          }}
        />
      </div>
      <Tip label={tr("resources.openExternal")}>
        <Button
          type="button"
          variant="ghost" size="icon-md"
          disabled={!ready}
          onClick={openExternal}
        >
          <IconExternalLink size={14} />
        </Button>
      </Tip>
    </div>
  );

  if (!desktop) {
    return (
      <div className="embedded-browser">
        {addressBar}
        <div className="embedded-browser__host">
          <div className="rp-preview__msg">{tr("resources.browserDesktopOnly")}</div>
        </div>
      </div>
    );
  }

  return (
    <div className="embedded-browser">
      {addressBar}
      {/* 宿主矩形：原生子 WebView 精确覆盖该区域。 */}
      <div
        ref={hostRef}
        className="embedded-browser__host"
        data-ready={ready ? "1" : "0"}
        aria-label={title || url}
      >
        {error ? (
          <Alert variant="error"><AlertDescription>{error}</AlertDescription></Alert>
        ) : ready ? (
          <div className="embedded-browser__host-fill" aria-hidden />
        ) : (
          <div className="rp-preview__msg">{tr("resources.loading")}</div>
        )}
      </div>
    </div>
  );
}

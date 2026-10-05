import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from "react";
import {
  TID_BROWSER_WEBVIEW,
  type NativeBrowserOwner,
  type NativeBrowserStateEvent,
  type NativeBrowserTarget,
} from "@zcode/shared";
import { cn } from "@/components/lib/utils.js";
import {
  BrowserEmptyState,
  BrowserLoadErrorState,
  BrowserToolbar,
} from "@/EmbeddedBrowserPaneParts.js";
import { BrowserViewportToolbar } from "@/browser-use/BrowserViewportToolbar.js";
import { ResponsiveBrowserViewport } from "@/browser-use/ResponsiveBrowserViewport.js";
import { DEFAULT_RESPONSIVE_BROWSER_VIEWPORT_SIZE } from "@/browser-use/ResponsiveBrowserViewport.js";
import { DEFAULT_BROWSER_VIEWPORT_ZOOM, type BrowserViewportZoom } from "@/browser-use/browserViewportZoom.js";
import { usePlatform } from "@/hooks/usePlatform.js";
import { useZCodeIntl } from "@/i18n/IntlProvider.js";
import { logger } from "@/logger.js";
import {
  registerNativeBrowserTarget,
  unregisterNativeBrowserTarget,
} from "@/browser-use/nativeBrowserTargetRegistry.js";
import {
  DEFAULT_BROWSER_URL,
  INITIAL_BROWSER_STATE,
  displayBrowserUrl,
  isDefaultBrowserOpenableUrl,
  normalizeBrowserUrl,
  type BrowserState,
} from "@/embeddedBrowserHelpers.js";
import type {
  HumanBrowserViewportPreferenceChangeSource,
  UnifiedBrowserViewProps,
} from "@/browser-use/UnifiedBrowserView.js";

type NativeBounds = { x: number; y: number; width: number; height: number };

function readBounds(element: HTMLElement | null): NativeBounds | null {
  if (!element) return null;
  const rect = element.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return null;
  return {
    x: Math.round(rect.left),
    y: Math.round(rect.top),
    width: Math.round(rect.width),
    height: Math.round(rect.height),
  };
}

function sameBounds(left: NativeBounds | null, right: NativeBounds | null): boolean {
  return (
    left?.x === right?.x &&
    left?.y === right?.y &&
    left?.width === right?.width &&
    left?.height === right?.height
  );
}

function readNativeZoom(
  element: HTMLElement | null,
  isResponsiveMode: boolean,
  zoom: BrowserViewportZoom,
): number {
  if (!isResponsiveMode) return 1;
  if (zoom !== "fit") return Number(zoom) / 100;
  const measured = Number(element?.dataset.responsiveScale);
  return Number.isFinite(measured) && measured > 0 ? measured : 1;
}

const BLOCKING_OVERLAY_SELECTORS = [
  '[role="dialog"]',
  '[data-slot="dialog-overlay"]',
  '[data-slot="alert-dialog-overlay"]',
  "[data-radix-popper-content-wrapper]",
  '[data-radix-menu-content][data-state="open"]',
  '[data-radix-dropdown-menu-content][data-state="open"]',
  '[data-radix-context-menu-content][data-state="open"]',
  '[data-radix-popover-content][data-state="open"]',
  '[data-radix-select-content][data-state="open"]',
].join(",");

function hasVisibleBlockingOverlay(surface: HTMLElement | null): boolean {
  if (typeof document === "undefined") return false;
  return Array.from(document.querySelectorAll<HTMLElement>(BLOCKING_OVERLAY_SELECTORS)).some(
    (element) => {
      if (surface?.contains(element) || element.getAttribute("aria-hidden") === "true") {
        return false;
      }
      const style = window.getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== "none" && style.visibility !== "hidden" && rect.width > 0 && rect.height > 0;
    },
  );
}

function sameNativeBrowserOwner(left: NativeBrowserOwner, right: NativeBrowserOwner): boolean {
  return (
    left.workspaceKey === right.workspaceKey &&
    left.sessionId === right.sessionId &&
    left.browserGeneration === right.browserGeneration
  );
}

/**
 * Tauri 专用浏览器视图。网页像素由 Rust 创建的真实子 WebView 绘制，renderer 只保留
 * 工具栏、错误态和宿主区域测量；页面协议由 Rust child WebView 直接承载。
 */
export function NativeBrowserView({
  browserKey,
  isVisible,
  initialUrl,
  navigationRequest,
  onUrlChange,
  onOpenBrowserUrl,
  onPageMetadataChange,
  onNavigationRequestHandled,
  onHumanViewportPreferenceChange,
  initialHumanViewportPreference,
  workspaceIdentity,
  workspaceKey,
  sessionId,
  browserGeneration,
}: UnifiedBrowserViewProps): React.JSX.Element {
  const platform = usePlatform();
  const { intl } = useZCodeIntl();
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const isVisibleRef = useRef(isVisible);
  const onOpenBrowserUrlRef = useRef(onOpenBrowserUrl);
  isVisibleRef.current = isVisible;
  onOpenBrowserUrlRef.current = onOpenBrowserUrl;
  const createdRef = useRef(false);
  const nativeTargetRef = useRef<NativeBrowserTarget | null>(null);
  const queueRef = useRef(Promise.resolve());
  const surfaceRevisionRef = useRef(0);
  const ownerKeyRef = useRef<string | null>(null);
  const lastBoundsRef = useRef<NativeBounds | null>(null);
  const lastNativeZoomRef = useRef<number | null>(null);
  const lastShownRef = useRef(false);
  const currentUrlRef = useRef(initialUrl || DEFAULT_BROWSER_URL);
  const initialUrlRef = useRef<string | null>(null);
  const appliedNavigationRequestIdRef = useRef<string | null>(null);
  const hasInitialNavigation = Boolean(initialUrl && initialUrl !== DEFAULT_BROWSER_URL);
  const [addressValue, setAddressValue] = useState(displayBrowserUrl(currentUrlRef.current));
  const [browserState, setBrowserState] = useState<BrowserState>(() => ({
    ...INITIAL_BROWSER_STATE,
    currentUrl: currentUrlRef.current,
    isLoading: hasInitialNavigation,
  }));
  const [hasBlockingOverlay, setHasBlockingOverlay] = useState(false);
  const [hasNavigated, setHasNavigated] = useState(hasInitialNavigation);
  const [isResponsiveMode, setIsResponsiveMode] = useState(
    initialHumanViewportPreference?.mode === "responsive",
  );
  const [responsiveViewportSize, setResponsiveViewportSize] = useState(
    initialHumanViewportPreference?.viewport ?? DEFAULT_RESPONSIVE_BROWSER_VIEWPORT_SIZE,
  );
  const [responsiveViewportZoom, setResponsiveViewportZoom] = useState<BrowserViewportZoom>(
    initialHumanViewportPreference?.zoom ?? DEFAULT_BROWSER_VIEWPORT_ZOOM,
  );

  const nativeOwner = useMemo<NativeBrowserOwner>(
    () => ({
      workspaceKey: workspaceKey?.trim() || workspaceIdentity?.trim() || "unscoped",
      sessionId: sessionId?.trim() || "unscoped",
      ...(browserGeneration !== undefined ? { browserGeneration } : {}),
    }),
    [browserGeneration, sessionId, workspaceIdentity, workspaceKey],
  );
  const nativeOwnerKey = useMemo(() => JSON.stringify(nativeOwner), [nativeOwner]);

  const enqueue = useCallback((operation: () => Promise<void>): Promise<void> => {
    const next = queueRef.current.catch(() => undefined).then(operation);
    queueRef.current = next;
    return next;
  }, []);

  const syncSurface = useCallback(
    (url = currentUrlRef.current): Promise<void> => {
      const revision = ++surfaceRevisionRef.current;
      return enqueue(async () => {
        if (revision !== surfaceRevisionRef.current) return;
        const bounds = readBounds(surfaceRef.current);
        const shouldShow =
          isVisible && hasNavigated && !hasBlockingOverlay && !browserState.errorMessage && bounds !== null;
        if (!shouldShow) {
          const target = nativeTargetRef.current;
          if (target && lastShownRef.current) {
            await platform.nativeBrowserHide?.(target);
            lastShownRef.current = false;
          }
          return;
        }

        if (!createdRef.current || !nativeTargetRef.current) {
          if (!platform.nativeBrowserOpen) return;
          const opened = await platform.nativeBrowserOpen({
            tabId: browserKey,
            url,
            bounds,
            owner: nativeOwner,
          });
          if (!Number.isSafeInteger(opened.generation) || opened.generation <= 0) {
            throw new Error("原生浏览器未返回有效 child generation");
          }
          const openedTarget: NativeBrowserTarget = {
            tabId: browserKey,
            owner: nativeOwner,
            generation: opened.generation,
          };
          if (revision !== surfaceRevisionRef.current) {
            await platform.nativeBrowserClose?.(openedTarget);
            return;
          }
          nativeTargetRef.current = openedTarget;
          registerNativeBrowserTarget(openedTarget);
          createdRef.current = true;
        }
        const target = nativeTargetRef.current;
        if (!target) return;
        if (revision !== surfaceRevisionRef.current) {
          if (lastShownRef.current) {
            await platform.nativeBrowserHide?.(target);
            lastShownRef.current = false;
          }
          return;
        }
        const nativeZoom = readNativeZoom(surfaceRef.current, isResponsiveMode, responsiveViewportZoom);
        if (lastNativeZoomRef.current !== nativeZoom) {
          await platform.nativeBrowserZoom?.(target, nativeZoom);
          lastNativeZoomRef.current = nativeZoom;
        }
        if (createdRef.current && !sameBounds(lastBoundsRef.current, bounds)) {
          await platform.nativeBrowserBounds?.({ ...target, bounds });
        }
        lastBoundsRef.current = bounds;
        if (!lastShownRef.current && revision === surfaceRevisionRef.current) {
          await platform.nativeBrowserShow?.(target);
          lastShownRef.current = true;
        }
      }).catch((error) => {
        setBrowserState((previous) => ({
          ...previous,
          errorMessage: error instanceof Error ? error.message : String(error),
          isLoading: false,
        }));
        logger.warn("[native-browser] 子 WebView 同步失败", {
          error: error instanceof Error ? error.message : String(error),
          tabId: browserKey,
        });
      });
    },
    [
      browserKey,
      browserState.errorMessage,
      enqueue,
      hasBlockingOverlay,
      hasNavigated,
      isVisible,
      platform,
      isResponsiveMode,
      responsiveViewportZoom,
      nativeOwner,
    ],
  );

  const updateNavigationState = useCallback(() => {
    const target = nativeTargetRef.current;
    const readState = target ? platform.nativeBrowserNavigationState?.(target) : undefined;
    if (!readState) return;
    void readState
      .then((navigation) => {
        setBrowserState((previous) => ({ ...previous, ...navigation, isReady: true }));
      })
      .catch((error) => {
        logger.debug("[native-browser] 读取导航历史失败", {
          error: error instanceof Error ? error.message : String(error),
          tabId: browserKey,
        });
      });
  }, [browserKey, platform]);

  const reportHumanViewportPreference = useCallback(
    (
      mode: "normal" | "responsive",
      viewport: typeof responsiveViewportSize,
      zoom: BrowserViewportZoom,
      source: HumanBrowserViewportPreferenceChangeSource,
    ) => {
      if (!initialHumanViewportPreference || !onHumanViewportPreferenceChange) return;
      onHumanViewportPreferenceChange({ mode, viewport: { ...viewport }, zoom }, source);
    },
    [initialHumanViewportPreference, onHumanViewportPreferenceChange],
  );

  const openUrl = useCallback(
    async (input: string, source: "toolbar" | "restore" | "request" = "toolbar") => {
      const nextUrl = normalizeBrowserUrl(input);
      if (!nextUrl) {
        setBrowserState((previous) => ({
          ...previous,
          errorMessage: intl.formatMessage({ id: "browser.invalidUrl" }),
        }));
        return;
      }
      currentUrlRef.current = nextUrl;
      setAddressValue(displayBrowserUrl(nextUrl));
      setHasNavigated(true);
      setBrowserState((previous) => ({
        ...previous,
        currentUrl: nextUrl,
        errorMessage: null,
        isLoading: true,
      }));
      onUrlChange?.(nextUrl);
      try {
        const target = nativeTargetRef.current;
        if (createdRef.current && target && platform.nativeBrowserNavigate) {
          await platform.nativeBrowserNavigate(target, nextUrl);
        } else {
          await syncSurface(nextUrl);
        }
      } catch (error) {
        setBrowserState((previous) => ({
          ...previous,
          errorMessage: intl.formatMessage(
            { id: "browser.loadFailed" },
            { message: error instanceof Error ? error.message : String(error) },
          ),
          isLoading: false,
        }));
      }
      if (source === "request") updateNavigationState();
    },
    [browserKey, intl, onUrlChange, platform, syncSurface, updateNavigationState],
  );

  useLayoutEffect(() => {
    if (ownerKeyRef.current === null) {
      ownerKeyRef.current = nativeOwnerKey;
      return;
    }
    if (ownerKeyRef.current === nativeOwnerKey) return;

    const staleTarget = nativeTargetRef.current;
    ownerKeyRef.current = nativeOwnerKey;
    surfaceRevisionRef.current += 1;
    nativeTargetRef.current = null;
    createdRef.current = false;
    lastShownRef.current = false;
    lastBoundsRef.current = null;
    lastNativeZoomRef.current = null;
    if (!staleTarget) return;
    unregisterNativeBrowserTarget(staleTarget);

    void enqueue(async () => {
      await platform.nativeBrowserClose?.(staleTarget);
    }).catch((error) => {
      logger.debug("[native-browser] owner 换代时关闭旧子 WebView 失败", {
        error: error instanceof Error ? error.message : String(error),
        tabId: browserKey,
      });
    });
  }, [browserKey, enqueue, nativeOwnerKey, platform]);

  useEffect(() => {
    const dispose = platform.onNativeBrowserState?.((event: NativeBrowserStateEvent) => {
      const target = nativeTargetRef.current;
      if (
        event.tabId !== browserKey ||
        !target ||
        event.generation !== target.generation ||
        !sameNativeBrowserOwner(event.owner, target.owner)
      ) {
        return;
      }
      if (event.kind === "new-window") {
        // Rust 已拒绝 child WebView 的独立窗口；只有当前可见 tab 才能把目标 URL
        // 交给主界面打开，后台 tab 的弹窗保持被拒绝，避免脱离 tab 生命周期。
        if (isVisibleRef.current && event.url) onOpenBrowserUrlRef.current?.(event.url);
        return;
      }
      const nextUrl = event.url || DEFAULT_BROWSER_URL;
      currentUrlRef.current = nextUrl;
      setAddressValue(displayBrowserUrl(nextUrl));
      setHasNavigated(nextUrl !== DEFAULT_BROWSER_URL);
      setBrowserState((previous) => ({
        ...previous,
        currentUrl: nextUrl,
        isLoading: event.kind === "started",
        isReady: true,
        ...(event.title === undefined ? {} : { title: event.title }),
      }));
      if (nextUrl !== DEFAULT_BROWSER_URL && event.kind !== "title") onUrlChange?.(nextUrl);
      if (event.title !== undefined) onPageMetadataChange?.({ title: event.title || undefined });
      if (event.kind === "finished") updateNavigationState();
    });
    return dispose;
  }, [
    browserKey,
    isVisible,
    onOpenBrowserUrl,
    onPageMetadataChange,
    onUrlChange,
    platform,
    updateNavigationState,
  ]);

  useLayoutEffect(() => {
    const element = surfaceRef.current;
    if (!element) return;
    const resizeObserver = new ResizeObserver(() => {
      void syncSurface();
    });
    resizeObserver.observe(element);
    const handleWindowResize = () => void syncSurface();
    window.addEventListener("resize", handleWindowResize);
    void syncSurface();
    return () => {
      resizeObserver.disconnect();
      window.removeEventListener("resize", handleWindowResize);
    };
  }, [syncSurface]);

  useEffect(() => {
    const updateOverlayState = () => {
      setHasBlockingOverlay(hasVisibleBlockingOverlay(surfaceRef.current));
    };
    updateOverlayState();
    if (typeof document === "undefined") return;
    const observer = new MutationObserver(updateOverlayState);
    observer.observe(document.body, {
      attributeFilter: ["aria-hidden", "class", "data-state", "style"],
      attributes: true,
      childList: true,
      subtree: true,
    });
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    void syncSurface();
  }, [hasBlockingOverlay, hasNavigated, isVisible, syncSurface]);

  useEffect(() => {
    if (!initialUrl || initialUrl === DEFAULT_BROWSER_URL || initialUrlRef.current === initialUrl) {
      return;
    }
    initialUrlRef.current = initialUrl;
    const normalized = normalizeBrowserUrl(initialUrl);
    if (normalized) void openUrl(normalized, "restore");
  }, [initialUrl, openUrl]);

  useEffect(() => {
    if (
      !navigationRequest ||
      appliedNavigationRequestIdRef.current === navigationRequest.id
    ) {
      return;
    }

    // 父层要等异步导航完成后才清除 request。openUrl 会更新地址、恢复 URL 和
    // 浏览器状态，期间会让回调身份变化并重新执行本 effect；同一个 request
    // 只能消费一次，否则重复导航会形成 React 最大更新深度循环。
    appliedNavigationRequestIdRef.current = navigationRequest.id;
    void openUrl(navigationRequest.url, "request").finally(() => {
      onNavigationRequestHandled?.(navigationRequest.id);
    });
  }, [navigationRequest, onNavigationRequestHandled, openUrl]);

  useEffect(
    () => () => {
      surfaceRevisionRef.current += 1;
      const target = nativeTargetRef.current;
      nativeTargetRef.current = null;
      createdRef.current = false;
      if (!target) return;
      unregisterNativeBrowserTarget(target);
      const closeRequest = platform.nativeBrowserClose?.(target);
      void closeRequest?.catch((error) => {
        logger.debug("[native-browser] 关闭子 WebView 失败", {
          error: error instanceof Error ? error.message : String(error),
          tabId: browserKey,
        });
      });
    },
    [browserKey, platform],
  );

  const handleSubmit = useCallback(
    (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      void openUrl(addressValue);
    },
    [addressValue, openUrl],
  );

  const handleGoBack = useCallback(() => {
    if (!browserState.canGoBack) return;
    const target = nativeTargetRef.current;
    if (!target) return;
    void platform.nativeBrowserHistory?.(target, "back");
  }, [browserState.canGoBack, platform]);

  const handleGoForward = useCallback(() => {
    if (!browserState.canGoForward) return;
    const target = nativeTargetRef.current;
    if (!target) return;
    void platform.nativeBrowserHistory?.(target, "forward");
  }, [browserState.canGoForward, platform]);

  const handleReload = useCallback(() => {
    if (!browserState.isReady) return;
    const target = nativeTargetRef.current;
    if (!target) return;
    void platform.nativeBrowserReload?.(target);
  }, [browserState.isReady, platform]);

  const handleOpenExternal = useCallback(() => {
    if (browserState.isReady && isDefaultBrowserOpenableUrl(browserState.currentUrl)) {
      platform.openExternal(browserState.currentUrl);
    }
  }, [browserState.currentUrl, browserState.isReady, platform]);

  const handleToggleResponsiveMode = useCallback(() => {
    const nextMode = !isResponsiveMode;
    const nextZoom = nextMode ? DEFAULT_BROWSER_VIEWPORT_ZOOM : responsiveViewportZoom;
    setIsResponsiveMode(nextMode);
    if (nextMode) setResponsiveViewportZoom(DEFAULT_BROWSER_VIEWPORT_ZOOM);
    reportHumanViewportPreference(
      nextMode ? "responsive" : "normal",
      responsiveViewportSize,
      nextZoom,
      "mode",
    );
    void syncSurface();
  }, [
    isResponsiveMode,
    reportHumanViewportPreference,
    responsiveViewportSize,
    responsiveViewportZoom,
    syncSurface,
  ]);

  const handleViewportSizeChange = useCallback(
    (viewport: typeof responsiveViewportSize) => {
      setResponsiveViewportSize(viewport);
      reportHumanViewportPreference(
        isResponsiveMode ? "responsive" : "normal",
        viewport,
        responsiveViewportZoom,
        "viewport",
      );
      void syncSurface();
    },
    [isResponsiveMode, reportHumanViewportPreference, responsiveViewportZoom, syncSurface],
  );

  const handleZoomChange = useCallback(
    (zoom: BrowserViewportZoom) => {
      setResponsiveViewportZoom(zoom);
      reportHumanViewportPreference(
        isResponsiveMode ? "responsive" : "normal",
        responsiveViewportSize,
        zoom,
        "zoom",
      );
      // 缩放比率由 Rust child WebView 应用，不能只更新 toolbar 偏好；
      // syncSurface 会在队列中按新 ratio 调用 browser_zoom。
      void syncSurface();
    },
    [isResponsiveMode, reportHumanViewportPreference, responsiveViewportSize, syncSurface],
  );

  const isEmptyBrowserState = !hasNavigated && !browserState.isLoading && !browserState.errorMessage;
  const showNativeSurface = isVisible && hasNavigated && !browserState.errorMessage;

  return (
    <div
      aria-hidden={!isVisible}
      className={cn(
        isVisible ? "flex" : "hidden",
        "h-full min-h-0 w-full min-w-0 flex-col overflow-hidden bg-background",
      )}
    >
      <BrowserToolbar
        addressValue={addressValue}
        browserState={browserState}
        formatMessage={intl.formatMessage}
        onAddressChange={setAddressValue}
        onGoBack={handleGoBack}
        onGoForward={handleGoForward}
        onOpenExternal={handleOpenExternal}
        onReload={handleReload}
        onToggleResponsiveMode={handleToggleResponsiveMode}
        onSubmit={handleSubmit}
        isElementPickerActive={false}
        isResponsiveMode={isResponsiveMode}
        showElementPicker={false}
        showDevTools={false}
      />
      {isResponsiveMode ? (
        <BrowserViewportToolbar
          isVisible={isVisible}
          onViewportSizeChange={handleViewportSizeChange}
          onZoomChange={handleZoomChange}
          viewportSize={responsiveViewportSize}
          zoom={responsiveViewportZoom}
        />
      ) : null}
      <div className="relative min-h-0 min-w-0 flex-1 overflow-hidden bg-background">
        {browserState.errorMessage ? (
          <BrowserLoadErrorState
            errorMessage={browserState.errorMessage}
            formatMessage={intl.formatMessage}
            onRetry={() => void openUrl(currentUrlRef.current)}
          />
        ) : isEmptyBrowserState ? (
          <BrowserEmptyState browserState={browserState} formatMessage={intl.formatMessage} />
        ) : null}
        <ResponsiveBrowserViewport
          active={isResponsiveMode}
          desktopZoomFactor={1}
          isComposed
          onResize={() => void syncSurface()}
          onViewportSizeChange={handleViewportSizeChange}
          viewportSize={responsiveViewportSize}
          zoom={responsiveViewportZoom}
        >
          <div
            ref={surfaceRef}
            aria-hidden={!showNativeSurface}
            data-testid={TID_BROWSER_WEBVIEW}
            className={cn("relative h-full w-full bg-background", showNativeSurface ? "block" : "hidden")}
          />
        </ResponsiveBrowserViewport>
      </div>
    </div>
  );
}
